use super::*;
use crate::{
    doubles::{Recorder, TestSession},
    test_support::*,
};
use bingo_sdk::{CommandSpec, Contribution, Plugin, Registrar, View};
use serde_json::json;
use std::sync::Arc;

fn contributions() -> Vec<Contribution> {
    let mut registrar = Registrar::new(
        "hello",
        json!({}),
        bingo_sdk::Env::rooted("/unused"),
        Default::default(),
    );
    bingo_mod_hello::Hello.register(&mut registrar).unwrap();
    registrar.into_contributions()
}
fn specs() -> Vec<CommandSpec> {
    contributions()
        .into_iter()
        .filter_map(|contribution| match contribution {
            Contribution::Command(command) if !command.spec().hidden => Some(command.spec()),
            _ => None,
        })
        .collect()
}
async fn drawing() -> View {
    let command = contributions()
        .into_iter()
        .find_map(|contribution| match contribution {
            Contribution::Command(command) if !command.spec().hidden => Some(command),
            _ => None,
        })
        .unwrap();
    let outcome = command
        .run(
            "",
            &bingo_sdk::CommandContext {
                session: state().summary.id,
                cwd: "/unused".into(),
                host: bingo_sdk::testing::NoHost::handle(),
                cancel: Default::default(),
            },
        )
        .await
        .unwrap();
    let bingo_sdk::CommandOutcome::View { view } = outcome else {
        panic!("view")
    };
    view
}
#[tokio::test]
async fn default_rebound_and_static_keys_use_the_command_catalogue() {
    for (settings, key, sends) in [
        (json!({}), ctrl('x'), true),
        (
            json!({"alt+h":"/hello"}),
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('h'),
                crossterm::event::KeyModifiers::ALT,
            ),
            true,
        ),
        (json!({"alt+h":"/hello"}), ctrl('x'), false),
        (json!({"ctrl+c":"/hello"}), ctrl('c'), false),
    ] {
        let home = tempfile::tempdir().unwrap();
        let mut state = state();
        state.config.kernel["keys"] = settings.clone();
        let session = Arc::new(TestSession::default());
        let now = scene().1;
        let mut run = tests::idle_in(state, session.clone(), now.instant);
        run.data_dir = home.path().into();
        run.ui.catalogs.commands = crate::commands::entries(specs());
        run.ui.composer.set("draft");
        site_keys::notices(&mut run);
        let mut screen = Recorder::default();
        run.paint(&mut screen, Wake::Frame, now).unwrap();
        if settings.get("ctrl+c").is_some() {
            assert!(
                screen.last().contains("belongs to the surface"),
                "{}",
                screen.last()
            );
        }
        run.terminal_event(Term::Key(key));
        assert_eq!(session.submitted().len(), usize::from(sends));
        if sends {
            assert!(matches!(&session.submitted()[0], Input::Text {text,..} if text == "/hello"));
            assert_eq!(run.ui.composer.text(), "draft");
        }
    }
}
fn pane(view: View) -> SessionState {
    let mut state = state();
    let slot: bingo_sdk::slot::Slot = serde_json::from_value(json!({"kind":"pane","id":"keys","title":"Keys","focus":true,"asked":true,"closeOnEscape":true,"holdToasts":false,"rows":8,"view":view})).unwrap();
    state.signals.entry("hello".into()).or_default().insert(
        "pane".into(),
        bingo_sdk::slot::with(json!({}), [slot]).into(),
    );
    state
}
#[tokio::test]
async fn focused_root_handler_gets_keys_and_unhandled_returns_to_controls() {
    let state = pane(drawing().await);
    let session = Arc::new(TestSession::default());
    let now = scene().1;
    let mut run = tests::idle_in(state, session.clone(), now.instant);
    let mut screen = Recorder::default();
    run.paint(&mut screen, Wake::Frame, now).unwrap();
    assert!(screen.last().contains("Hello from a built-in mod"));
    run.terminal_event(Term::Key(typed('x')));
    assert!(
        matches!(&session.submitted()[0], Input::Action { action, .. } if action.args["value"] == json!({"key":"x"}))
    );
    let intent = run.site_keys.keys().next().unwrap().clone();
    run.ack(
        &intent,
        &IntentOutcome::Applied {
            result: json!({"handled":false}),
        },
    );
    assert_eq!(run.ui.composer.text(), "x");
    for key in [key(crossterm::event::KeyCode::Esc), ctrl('c')] {
        run.ui.focus = crate::focus::Focus::Pane(format!(
            "{}/hello/keys",
            run.session.tree.viewed().summary.id
        ));
        run.terminal_event(Term::Key(key));
    }
    assert!(!session.submitted().iter().any(|input| matches!(input, Input::Action {action, ..} if action.args["value"]["key"] == "esc" || action.args["value"]["key"] == "ctrl+c")));
}
#[tokio::test]
async fn an_immediately_ready_tree_is_drawn_in_the_requesting_pass() {
    let mut state = state();
    state.config.kernel["render"] = json!({"Welcome":[{}]});
    let host = bingo_sdk::testing::TestHost::default();
    let view = drawing().await;
    host.on_render(move |_, _| {
        let view = view.clone();
        async move { Ok(view) }
    });
    let now = scene().1;
    let mut run = tests::idle(now.instant);
    run.session.tree = solo(&state);
    run.host = host.handle();
    let mut screen = Recorder::default();
    run.paint(&mut screen, Wake::Frame, now).unwrap();
    assert_eq!(screen.frames.len(), 1);
    assert!(
        screen.last().contains("Hello from a built-in mod"),
        "{}",
        screen.last()
    );
}

#[tokio::test]
async fn a_malformed_handler_cannot_panic_the_surface() {
    let mut view = drawing().await;
    if let View::Box { layout, .. } = &mut view {
        layout.on_key.as_mut().unwrap().args = json!("invalid");
    }
    let state = pane(view);
    let (mut ui, now) = scene();
    draw_sized(120, 40, &state, &mut ui, now);
    assert!(crate::pane::key_handler(&ui, &state, typed('x')).is_none());
}

#[tokio::test]
async fn a_key_names_only_itself_never_the_drawing_it_was_pressed_on() {
    let mut view = drawing().await;
    if let View::Box { layout, .. } = &mut view {
        layout.on_key.as_mut().unwrap().args["value"] = json!({"row":"model:a"});
    }
    let session = Arc::new(TestSession::default());
    let now = scene().1;
    let mut run = tests::idle_in(pane(view), session.clone(), now.instant);
    run.paint(&mut Recorder::default(), Wake::Frame, now)
        .unwrap();
    run.terminal_event(Term::Key(typed('x')));
    assert!(
        matches!(&session.submitted()[0], Input::Action { action, .. } if action.args["value"] == json!({"key":"x"})),
        "{:?}",
        session.submitted()
    );
}
#[tokio::test]
async fn escape_returns_from_aside_and_otherwise_stops_permission_asks() {
    for child_asks in [false, true] {
        for editing in ["list", "aside", "change"] {
            let root = Arc::new(TestSession::default());
            let child = Arc::new(TestSession::default());
            let now = still(scene().1);
            let mut run = tests::idle_in(state(), root.clone(), now.instant);
            let mut open = permission(None, None);
            let owner = if child_asks {
                child_id()
            } else {
                state().summary.id
            };
            open.session = owner.clone();
            run.session.tree = folded_tree(vec![child_frame(1, announced("child"))]);
            let event = opened(open.clone());
            run.session.tree.fold.apply(&if child_asks {
                child_frame(2, event)
            } else {
                frame(2, event)
            });
            run.session.tree.show(&if child_asks {
                state().summary.id
            } else {
                child_id()
            });
            run.session
                .handles
                .insert(child_id(), SessionHandle(child.clone()));
            run.ui.dialog.focus_on(Some(&open));
            if editing == "aside" {
                run.ui.dialog.aside = true;
            }
            if editing == "change" {
                run.ui.dialog.words = Some(Default::default());
            }
            let effects = input::on_key(
                &mut run.ui,
                &run.session.tree,
                key(crossterm::event::KeyCode::Esc),
                now,
            );
            run.apply(effects);
            let (asking, other) = if child_asks {
                (&child, &root)
            } else {
                (&root, &child)
            };
            assert_eq!(
                asking.interrupts(),
                usize::from(editing != "aside"),
                "{child_asks} {editing}"
            );
            assert_eq!(
                asking.answers().len(),
                usize::from(editing != "aside"),
                "{child_asks} {editing}"
            );
            assert_eq!(other.interrupts(), 0);
            assert!(other.answers().is_empty());
        }
    }
}

fn question_run() -> (Run, Arc<TestSession>, crate::clock::Now) {
    let mut state = folded(vec![frame(1, opened(question(false, true)))]);
    state.config.kernel["render"] = json!({"Question":[{}]});
    let session = Arc::new(TestSession::default());
    let now = still(scene().1);
    let mut run = tests::idle_in(state, session.clone(), now.instant);
    interactions::refocus(&mut run);
    (run, session, now)
}
fn pull_question(run: &mut Run, width: u16, now: crate::clock::Now) {
    draw_tree(width, 24, &run.session.tree, &mut run.ui, now);
    let key = run
        .ui
        .shown
        .pulls
        .iter()
        .find(|key| key.request.component == bingo_sdk::render::Component::Question)
        .unwrap()
        .clone();
    let props = serde_json::from_value(key.request.props.clone()).unwrap();
    let view = bingo_mod_ask::drawing::tree(
        &props,
        &Default::default(),
        &key.request.request_id,
        &key.session,
    );
    run.ui.placed.pulled.request(&key);
    run.ui.placed.pulled.answer(key, view);
}
#[tokio::test]
async fn a_fast_unhandled_burst_keeps_its_key_order() {
    let (mut run, session, now) = question_run();
    pull_question(&mut run, 108, now);
    for c in "hello".chars() {
        run.terminal_event(Term::Key(typed(c)));
    }
    assert_eq!(
        session.submitted().len(),
        1,
        "only one key may be in flight"
    );
    let intent = run.site_keys.keys().next().unwrap().clone();
    run.ack(
        &intent,
        &IntentOutcome::Applied {
            result: json!({"handled":false}),
        },
    );
    assert_eq!(run.ui.composer.text(), "hello");
}
#[tokio::test]
async fn resize_then_down_then_enter_uses_one_cursor() {
    let (mut run, session, now) = question_run();
    pull_question(&mut run, 108, now);
    draw_tree(80, 24, &run.session.tree, &mut run.ui, now);
    run.terminal_event(Term::Key(key(crossterm::event::KeyCode::Down)));
    run.terminal_event(Term::Key(key(crossterm::event::KeyCode::Enter)));
    assert!(
        session.answers().is_empty(),
        "wait for the mod before interpreting keys"
    );
    assert_eq!(
        run.ui.dialog.focus, 0,
        "the fallback must not move another cursor"
    );
    pull_question(&mut run, 80, now);
    typeahead::drain(&mut run);
    // The real mod's pure state interprets the requests in mailbox order.
    let mut state = bingo_mod_ask::state::State::default();
    let bingo_sdk::InteractionKind::Question(question) = question(false, true).kind else {
        panic!("question")
    };
    for index in 0..2 {
        let input = session
            .submitted()
            .get(index)
            .cloned()
            .expect("queued key dispatched");
        let Input::Action { action, .. } = input else {
            panic!("action")
        };
        let (handled, answer) = state.key(
            std::slice::from_ref(&question),
            false,
            action.args["value"]["key"].as_str().unwrap(),
        );
        let intent = run.site_keys.keys().next().unwrap().clone();
        run.ack(
            &intent,
            &IntentOutcome::Applied {
                result: json!({"handled":handled,"answer":answer}),
            },
        );
    }
    assert!(matches!(&session.answers()[0].1, bingo_sdk::Answer::Choice {ids,..} if ids == &["o"]));
}

#[tokio::test]
async fn aside_uses_surface_reception_instead_of_the_kernel_clock() {
    let now = later(scene().1, 3_600_000);
    let mut run = tests::idle_in(
        folded(vec![frame(1, opened(question(false, true)))]),
        Arc::new(TestSession::default()),
        now.instant,
    );
    run.ui.last_typed = Some(now.wall);
    interactions::refocus_at(&mut run, now.wall);
    assert!(
        run.ui.dialog.aside,
        "the fixture's remote opened_at is long before the local keystroke"
    );
}

#[tokio::test]
async fn a_question_key_can_finish_the_pane_action_waiting_for_its_answer() {
    let now = still(scene().1);
    let session = Arc::new(TestSession::default());
    let mut state = pane(drawing().await);
    state.config.kernel["render"] = json!({"Question":[{}]});
    let mut run = tests::idle_in(state, session.clone(), now.instant);
    let mut screen = Recorder::default();
    run.paint(&mut screen, Wake::Frame, now).unwrap();
    run.terminal_event(Term::Key(typed('x')));
    let pane_intent = session.submissions()[0].0.clone();
    assert!(run.ui.actions.action().is_some());

    // The pane command asks and cannot acknowledge until the person answers.
    let open = question(false, true);
    run.frame(&frame(1, opened(open.clone())), &mut screen)
        .unwrap();
    if run.ui.dialog.aside {
        run.terminal_event(Term::Key(ctrl('g')));
    }
    pull_question(&mut run, 108, now);
    run.paint(&mut screen, Wake::Frame, now).unwrap();
    assert!(
        screen.last().contains("Which provider?"),
        "{}",
        screen.last()
    );
    pull_question(&mut run, 108, now);
    run.terminal_event(Term::Key(typed('2')));
    let submissions = session.submissions();
    assert_eq!(
        submissions.len(),
        2,
        "the question must bypass its waiting caller"
    );
    assert!(
        run.site_keys.contains_key(&pane_intent),
        "caller still awaits its answer"
    );
    answer_key(&mut run, &submissions[1], &open);
    assert!(
        matches!(&session.answers()[0].1, bingo_sdk::Answer::Choice { ids, .. } if ids == &["o"])
    );
    assert!(
        run.ui.actions.action().is_some(),
        "answer ack cannot release the caller"
    );
    run.ack(
        &pane_intent,
        &IntentOutcome::Applied {
            result: json!({"handled":true}),
        },
    );
    assert!(run.ui.actions.action().is_none());
    assert!(run.site_keys.is_empty());
}

fn answer_key(run: &mut Run, submission: &(IntentId, Input), open: &bingo_sdk::Interaction) {
    let Input::Action { action, .. } = &submission.1 else {
        panic!("key action")
    };
    let bingo_sdk::InteractionKind::Question(question) = open.kind.clone() else {
        panic!("question")
    };
    let (handled, answer) = bingo_mod_ask::state::State::default().key(
        &[question],
        false,
        action.args["value"]["key"].as_str().unwrap(),
    );
    run.ack(
        &submission.0,
        &IntentOutcome::Applied {
            result: json!({"handled":handled,"answer":answer}),
        },
    );
}

#[tokio::test(start_paused = true)]
async fn escape_interrupts_even_without_an_offered_cancellation_answer() {
    for cached in [false, true] {
        let (host, root, child) = crate::doubles::TestHost::tree(Vec::new());
        let now = still(scene().1);
        let mut run = tests::idle_in(state(), root.clone(), now.instant);
        run.host = host;
        let (replies, mut waiting) = mpsc::channel(4);
        run.replies = replies;
        let mut open = question(false, false);
        open.session = child_id();
        open.answers = vec![bingo_sdk::AnswerSpec::Choice];
        open.guard_until = Some(now.wall + jiff::SignedDuration::from_secs(60));
        run.session.tree = folded_tree(vec![
            child_frame(1, announced("child")),
            child_frame(2, started("child-turn")),
            child_frame(3, opened(open)),
        ]);
        if cached {
            run.session
                .handles
                .insert(child_id(), SessionHandle(child.clone()));
        }
        interactions::refocus(&mut run);
        let effects = input::on_key(
            &mut run.ui,
            &run.session.tree,
            key(crossterm::event::KeyCode::Esc),
            now,
        );
        assert!(
            !effects.is_empty(),
            "Escape must interrupt without an answer to send"
        );
        run.apply(effects);
        if !cached {
            let reply = tokio::time::timeout(Duration::from_secs(1), waiting.recv())
                .await
                .unwrap()
                .unwrap();
            run.reply(reply, &mut None);
        }
        assert_eq!(child.interrupts(), 1, "cached: {cached}");
        assert!(
            child.answers().is_empty(),
            "do not invent an unsupported answer"
        );
        assert_eq!(root.interrupts(), 0);
        assert_eq!(
            run.ui.stop_asked,
            Some(bingo_sdk::TurnId::from_raw("child-turn"))
        );
    }
}

fn cross_session_question(run: &mut Run, child_asks: bool) -> bingo_sdk::Interaction {
    let mut open = question(false, true);
    let stamp = if child_asks { child_frame } else { frame };
    open.session = if child_asks {
        child_id()
    } else {
        state().summary.id
    };
    run.session.tree = folded_tree(vec![
        child_frame(1, announced("child")),
        stamp(
            2,
            Event::ConfigChanged {
                config: bingo_sdk::ConfigView {
                    kernel: json!({"render":{"Question":[{}]}}),
                    ..Default::default()
                },
            },
        ),
        stamp(3, opened(open.clone())),
    ]);
    run.session.tree.show(&if child_asks {
        state().summary.id
    } else {
        child_id()
    });
    interactions::refocus(run);
    open
}

async fn answer_across_sessions(child_asks: bool, cached: bool) {
    let (host, root, child) = crate::doubles::TestHost::tree(Vec::new());
    let now = still(scene().1);
    let mut run = tests::idle_in(state(), root.clone(), now.instant);
    run.host = host;
    let (replies, mut waiting) = mpsc::channel(4);
    run.replies = replies;
    let open = cross_session_question(&mut run, child_asks);
    if cached {
        run.session
            .handles
            .insert(child_id(), SessionHandle(child.clone()));
    }
    pull_question(&mut run, 108, now);
    run.terminal_event(Term::Key(typed('2')));
    assert_eq!(
        run.site_keys.len(),
        1,
        "retain the key for its asking session"
    );
    if child_asks && !cached {
        let reply = tokio::time::timeout(Duration::from_secs(1), waiting.recv())
            .await
            .unwrap()
            .unwrap();
        run.reply(reply, &mut None);
    }
    let (asking, other) = if child_asks {
        (&child, &root)
    } else {
        (&root, &child)
    };
    assert_eq!(asking.submitted().len(), 1, "send to the ask's owner");
    assert!(
        other.submitted().is_empty(),
        "the viewed session must not receive the key"
    );
    answer_key(&mut run, &asking.submissions()[0], &open);
    assert!(matches!(&root.answers()[0].1, bingo_sdk::Answer::Choice { ids, .. } if ids == &["o"]));
}

#[tokio::test(start_paused = true)]
async fn a_root_question_accepts_keys_while_viewing_a_child_without_a_writer() {
    answer_across_sessions(false, false).await;
}

#[tokio::test(start_paused = true)]
async fn a_child_question_uses_its_owner_handle_including_before_it_arrives() {
    for cached in [true, false] {
        answer_across_sessions(true, cached).await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_failed_owner_open_releases_the_pending_question_key() {
    let (mut run, session, now) = question_run();
    cross_session_question(&mut run, true);
    let host = bingo_sdk::testing::TestHost::default();
    host.on_open(|_, _, _| async {
        Err(KernelError::new(
            bingo_sdk::ErrorCode::NotFound,
            "fixture owner closed",
        ))
    });
    run.host = host.handle();
    let (replies, mut waiting) = mpsc::channel(4);
    run.replies = replies;
    pull_question(&mut run, 108, now);
    run.terminal_event(Term::Key(typed('2')));
    assert_eq!(run.site_keys.len(), 1);
    let reply = tokio::time::timeout(Duration::from_secs(1), waiting.recv())
        .await
        .unwrap()
        .unwrap();
    run.reply(reply, &mut None);
    assert!(run.site_keys.is_empty());
    assert!(run.mine.is_empty());
    assert!(session.submitted().is_empty());
    assert!(session.answers().is_empty());
}

#[tokio::test(start_paused = true)]
async fn escape_discards_keys_waiting_for_the_asking_sessions_handle() {
    let (host, root, child) = crate::doubles::TestHost::tree(Vec::new());
    let now = still(scene().1);
    let mut run = tests::idle_in(state(), root.clone(), now.instant);
    run.host = host;
    let (replies, mut waiting) = mpsc::channel(4);
    run.replies = replies;
    cross_session_question(&mut run, true);
    pull_question(&mut run, 108, now);
    run.terminal_event(Term::Key(key(crossterm::event::KeyCode::Down)));
    run.terminal_event(Term::Key(key(crossterm::event::KeyCode::Enter)));
    let pending = run.site_keys.keys().next().unwrap().clone();
    assert_eq!(run.typeahead.len(), 1);
    run.terminal_event(Term::Key(key(crossterm::event::KeyCode::Esc)));
    assert!(
        run.site_keys.is_empty(),
        "discard the key whose mailbox is still opening"
    );
    assert!(
        run.typeahead.is_empty(),
        "discard its queued follow-up key too"
    );
    for _ in 0..1 {
        let reply = tokio::time::timeout(Duration::from_secs(1), waiting.recv())
            .await
            .unwrap()
            .unwrap();
        run.reply(reply, &mut None);
    }
    run.ack(
        &pending,
        &IntentOutcome::Applied {
            result: json!({"handled":false}),
        },
    );
    assert!(
        child.submitted().is_empty(),
        "a late open must not revive the key"
    );
    assert_eq!(child.interrupts(), 0);
    assert!(child.answers().is_empty());
    assert_eq!(root.answers()[0].1, bingo_sdk::Answer::Cancel);
}

#[tokio::test(start_paused = true)]
async fn a_late_owner_handle_cannot_revive_a_resolved_question() {
    let (host, root, child) = crate::doubles::TestHost::tree(Vec::new());
    let now = still(scene().1);
    let mut run = tests::idle_in(state(), root, now.instant);
    run.host = host;
    let (replies, mut waiting) = mpsc::channel(4);
    run.replies = replies;
    let open = cross_session_question(&mut run, true);
    pull_question(&mut run, 108, now);
    run.terminal_event(Term::Key(typed('2')));
    run.frame(
        &child_frame(
            4,
            Event::InteractionResolved {
                id: open.id,
                answer: bingo_sdk::Answer::Choice {
                    ids: vec!["a".into()],
                    other: None,
                },
                by: bingo_sdk::ResolvedBy::Kernel,
            },
        ),
        &mut Recorder::default(),
    )
    .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(1), waiting.recv())
        .await
        .unwrap()
        .unwrap();
    run.reply(reply, &mut None);
    assert!(
        child.submitted().is_empty(),
        "the answer may have arrived from another surface"
    );
    assert!(run.site_keys.is_empty());
    assert!(run.mine.is_empty());
}

async fn switch(host: &bingo_core::Host, attachment: &mut bingo_sdk::Attachment, enabled: bool) {
    use futures::StreamExt;
    let intent = bingo_sdk::IntentId::mint();
    attachment.handle.submit(
        intent.clone(),
        Input::text(
            format!(
                "/plugins {} hello",
                if enabled { "enable" } else { "disable" }
            ),
            bingo_sdk::Origin::surface("tui"),
        ),
    );
    loop {
        let frame = attachment.events.next().await.unwrap();
        if let bingo_sdk::Event::IntentAck {
            intent: ack,
            outcome,
        } = frame.event
            && ack == intent
        {
            assert!(
                matches!(outcome, IntentOutcome::Applied { .. }),
                "{outcome:?}"
            );
            break;
        }
    }
    let catalogue = bingo_sdk::HostApi::catalog(host, CatalogKind::Commands)
        .await
        .unwrap();
    assert_eq!(
        catalogue.entries.iter().any(|entry| entry.id == "hello"),
        enabled
    );
}

async fn live_hello(home: &std::path::Path) -> (Arc<bingo_core::Host>, bingo_sdk::Attachment) {
    use bingo_sdk::{
        ClientIdentity, Driver, Env, HostApi, OpenOptions, SessionSelector, SessionSpec,
    };
    let host = bingo_core::Host::build(
        vec![Box::new(bingo_mod_hello::Hello)],
        bingo_core::HostConfig::new(Env::rooted(home)),
    )
    .await
    .unwrap();
    let attachment = host
        .open(
            SessionSelector::Create {
                spec: SessionSpec {
                    driver: Driver::Log,
                    cwd: home.into(),
                    ..Default::default()
                },
            },
            ClientIdentity {
                name: "test".into(),
                surface: "terminal".into(),
            },
            OpenOptions::default(),
        )
        .await
        .unwrap();
    (host, attachment)
}

fn toast_run(
    host: &bingo_core::Host,
    attachment: &bingo_sdk::Attachment,
    home: &std::path::Path,
    now: Now,
) -> (Run, Arc<TestSession>) {
    let mut state = attachment.snapshot.clone();
    state.config.kernel["render"] = json!({"Toast":[{}]});
    state.signals.entry("fixture".into()).or_default().insert(
        "toast".into(),
        bingo_sdk::slot::with(
            json!({}),
            [bingo_sdk::slot::Slot::Toast {
                text: "Surface fallback".into(),
                until: now.wall.checked_add(Duration::from_secs(60)).unwrap(),
                alert: false,
            }],
        )
        .into(),
    );
    let session = Arc::new(TestSession::default());
    let mut run = tests::idle_in(state, session.clone(), now.instant);
    run.host = host.handle();
    run.data_dir = home.into();
    (run, session)
}

#[tokio::test]
async fn plugin_switch_redraws_the_toast_and_refreshes_default_keys() {
    use bingo_sdk::HostApi;
    use futures::StreamExt;
    let home = tempfile::tempdir().unwrap();
    let (host, mut attachment) = live_hello(home.path()).await;
    let mut gateway = host.gateway_events();
    let now = scene().1;
    let (mut run, session) = toast_run(&host, &attachment, home.path(), now);
    let (sender, mut replies) = mpsc::channel(32);
    run.replies = sender;
    let mut screen = Recorder::default();
    for enabled in [true, false, true] {
        switch(&host, &mut attachment, enabled).await;
        loop {
            if let Some(bingo_sdk::GatewayEvent::CatalogChanged {
                kind: CatalogKind::Commands,
            }) = gateway.next().await
            {
                catalogues::changed(&mut run, CatalogKind::Commands);
                break;
            }
        }
        loop {
            let reply = replies.recv().await.unwrap();
            let commands = matches!(reply, Reply::Commands(..));
            run.reply(reply, &mut None);
            if commands {
                break;
            }
        }
        run.paint(&mut screen, Wake::Frame, now).unwrap();
        assert_eq!(
            screen.last().contains("Hello from a built-in mod"),
            enabled,
            "{}",
            screen.last()
        );
        assert_eq!(
            screen.last().contains("Surface fallback"),
            !enabled,
            "{}",
            screen.last()
        );
        let before = session.submitted().len();
        run.terminal_event(Term::Key(ctrl('x')));
        assert_eq!(session.submitted().len() - before, usize::from(enabled));
        // The kernel answers the command the key sent, as it does live.
        if let Some((intent, _)) = session.submissions().last().cloned() {
            run.ack(&intent, &IntentOutcome::Applied { result: json!({}) });
        }
    }
    host.shutdown().await;
}

#[tokio::test]
async fn a_synchronous_browse_projection_owes_the_frame_with_its_matches() {
    let now = scene().1;
    let mut state = state();
    state.config.kernel["render"] = json!({"Browse":[{}]});
    state.transcript = vec![user("i1", "needle")].into_iter().collect();
    let host = bingo_sdk::testing::TestHost::default();
    host.on_render(|_, request| async move {
        let mut props = request.props;
        props["query"] = json!("needle");
        Ok(View::Box {
            children: vec![View::text(format!(
                "matches: {}",
                props["count"].as_u64().unwrap()
            ))],
            layout: Box::new(bingo_sdk::view::BoxStyle {
                on_key: Some(bingo_sdk::Action {
                    name: "browse:ui".into(),
                    args: json!({"props":props}),
                }),
                ..Default::default()
            }),
        })
    });
    let mut run = tests::idle(now.instant);
    run.session.tree = solo(&state);
    run.host = host.handle();
    run.ui.catalogs.commands.push(crate::commands::entry(
        serde_json::from_value(
            json!({"name":"browse","hint":"browse","args":{"kind":"none"},"family":"terminal"}),
        )
        .unwrap(),
    ));
    crate::browse::open(&mut run.ui, &run.session.tree, None, false, now);
    let mut screen = Recorder::default();
    run.paint(&mut screen, Wake::Frame, now).unwrap();
    assert!(
        run.behind,
        "the synchronous final answer still needs painting"
    );
    assert!(run.next_draw().is_some());
    run.paint(&mut screen, Wake::Frame, now).unwrap();
    assert!(screen.last().contains("matches: 1"), "{}", screen.last());
    assert!(!run.behind, "a settled projection does not keep ticking");
}

#[tokio::test]
async fn a_login_opened_after_typing_keeps_paste_out_of_the_composer() {
    let mut state = state();
    let mut open = permission(None, None);
    open.kind = bingo_sdk::InteractionKind::Login {
        provider: "example".into(),
        flow: bingo_sdk::LoginFlow::Paste,
    };
    open.answers = vec![bingo_sdk::AnswerSpec::Text, bingo_sdk::AnswerSpec::Cancel];
    state.interactions = vec![open];
    let now = still(scene().1);
    let mut run = tests::idle_in(state, Arc::new(TestSession::default()), now.instant);
    run.ui.last_typed = Some(now.wall);
    interactions::refocus_at(&mut run, now.wall);
    assert!(!run.ui.dialog.aside);
    run.terminal_event(Term::Paste("sk-dummy-never-unmasked".into()));
    let mut screen = Recorder::default();
    run.paint(&mut screen, Wake::Frame, now).unwrap();
    assert!(!screen.last().contains("sk-dummy-never-unmasked"));
    assert_eq!(
        run.ui.dialog.words.as_ref().unwrap().text(),
        "sk-dummy-never-unmasked"
    );
    assert!(run.ui.composer.is_empty());
}

//! Keys typed ahead of the site that reads them are never lost (M180).
use super::*;
use crate::{doubles::TestSession, test_support::*};
use bingo_sdk::{Action, render::Component};
use crossterm::event::KeyCode;
use serde_json::json;
use std::sync::Arc;

/// Answers the open site's drawing with a view that sends its keys back.
fn pull(run: &mut Run, component: Component, now: crate::clock::Now) {
    draw_tree(80, 24, &run.session.tree, &mut run.ui, now);
    let Some(key) = run
        .ui
        .shown
        .pulls
        .iter()
        .find(|key| key.request.component == component)
        .cloned()
    else {
        return;
    };
    if run.ui.placed.pulled.get(&key).is_some() {
        return;
    }
    let view = View::Box {
        children: vec![View::text("site")],
        layout: Box::new(bingo_sdk::view::BoxStyle {
            on_key: Some(Action {
                name: "site:ui".into(),
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

/// The newest key action the site sent and nobody has answered yet.
fn unanswered(session: &TestSession, answered: &[IntentId]) -> Option<(IntentId, Action)> {
    session
        .submissions()
        .into_iter()
        .filter(|(intent, _)| !answered.contains(intent))
        .find_map(|(intent, input)| match input {
            Input::Action { action, .. } => Some((intent, action)),
            _ => None,
        })
}

/// Plays the palette and its kernel: every key is judged after the site has
/// drawn the query before it, and Enter runs the line the query names.
fn play_palette(run: &mut Run, session: &TestSession, now: crate::clock::Now) {
    let mut answered = Vec::new();
    for _ in 0..64 {
        pull(run, Component::Palette, now);
        let Some((intent, action)) = unanswered(session, &answered) else {
            break;
        };
        answered.push(intent.clone());
        let result = if action.args["value"]["key"] == "enter" {
            let handler = crate::palette::request(&run.ui).unwrap().to_owned();
            let line = action.args["query"].as_str().unwrap().to_owned();
            json!({"drawnBy":"palette","surface":{"name":"surface:palette","args":{"handler":handler,"value":{"run":line}}}})
        } else {
            json!({"handled":false})
        };
        run.ack(&intent, &IntentOutcome::Applied { result });
    }
}

fn texts(session: &TestSession) -> Vec<String> {
    session
        .submitted()
        .into_iter()
        .filter_map(|input| match input {
            Input::Text { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

fn palette_run(now: crate::clock::Now) -> (tempfile::TempDir, Run, Arc<TestSession>) {
    let mut state = state();
    state.config.kernel["render"] = json!({"Palette":[{}],"Sessions":[{}]});
    let session = Arc::new(TestSession::default());
    let mut run = tests::idle_in(state, session.clone(), now.instant);
    // A submitted line goes into the prompt history, kept out of the sources.
    let home = tempfile::tempdir().unwrap();
    run.data_dir = home.path().into();
    (home, run, session)
}

fn type_burst(run: &mut Run, burst: &str) {
    for letter in burst.chars() {
        let pressed = if letter == '\n' {
            key(KeyCode::Enter)
        } else {
            typed(letter)
        };
        run.terminal_event(Term::Key(pressed));
    }
}

#[tokio::test]
async fn a_command_typed_in_one_burst_runs_once_the_palette_answers() {
    let now = still(scene().1);
    let (_home, mut run, session) = palette_run(now);
    type_burst(&mut run, "/ledger\n");
    assert!(
        crate::palette::active(&run.ui),
        "the slash opened the palette"
    );
    assert!(
        session.submitted().is_empty(),
        "nothing judged before a drawing"
    );
    play_palette(&mut run, &session, now);
    assert_eq!(texts(&session), ["/ledger"]);
    assert!(run.typeahead.is_empty());
}

#[tokio::test]
async fn keys_typed_behind_a_command_reach_the_site_it_opens() {
    let now = still(scene().1);
    let (_home, mut run, session) = palette_run(now);
    run.ui
        .catalogs
        .commands
        .push(crate::commands::entry(bingo_sdk::CommandSpec {
            name: "sessions".into(),
            opens: true,
            keys: vec![],
            aliases: vec![],
            hint: String::new(),
            args: bingo_sdk::ArgSpec::None,
            instant: true,
            hidden: false,
            family: "session".into(),
        }));
    type_burst(&mut run, "/sessions\nabc");
    run.terminal_event(Term::Paste("X".into()));
    type_burst(&mut run, "de");
    play_palette(&mut run, &session, now);
    assert_eq!(
        texts(&session),
        ["/sessions"],
        "the line went to the kernel"
    );
    assert!(!crate::palette::active(&run.ui));
    assert_eq!(
        run.typeahead.len(),
        6,
        "keys and paste wait for the command's answer"
    );
    assert_eq!(run.ui.composer.text(), "");
    let (intent, _) = session.submissions().last().cloned().unwrap();
    let result = json!({"handled":true,"surface":{"name":"Sessions","args":{}}});
    run.ack(&intent, &IntentOutcome::Applied { result });
    assert!(
        crate::sessions::active(&run.ui),
        "the command opened its site"
    );
    let mut answered: Vec<IntentId> = session
        .submissions()
        .into_iter()
        .map(|(intent, _)| intent)
        .collect();
    for _ in 0..16 {
        // The directory's drawing carries the minute, so it is drawn now.
        let drawn = run.detection.now();
        pull(&mut run, Component::Sessions, drawn);
        let Some((intent, action)) = unanswered(&session, &answered) else {
            if run.typeahead.is_empty() {
                break;
            }
            continue; // A paste edits the filter without a key acknowledgement.
        };
        assert_eq!(
            action.args["query"],
            run.ui.composer.text(),
            "each key is judged against the filter it was typed after"
        );
        answered.push(intent.clone());
        let result = json!({"handled":false});
        run.ack(&intent, &IntentOutcome::Applied { result });
    }
    assert_eq!(
        run.ui.composer.text(),
        "abcXde",
        "the filter holds every input"
    );
    assert!(run.typeahead.is_empty());
}

#[tokio::test]
async fn keys_typed_before_a_dialog_opened_are_never_entered_into_it() {
    let now = still(scene().1);
    for ask in [permission(None, None), question(false, true)] {
        let (_home, mut run, session) = palette_run(now);
        type_burst(&mut run, "/");
        pull(&mut run, Component::Palette, now);
        type_burst(&mut run, "\nya");
        let (enter, _) = unanswered(&session, &[]).unwrap();
        assert_eq!(run.typeahead.len(), 2, "y and a wait behind the enter");
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
        let handler = crate::palette::request(&run.ui).unwrap().to_owned();
        let result = json!({"surface":{"name":"surface:palette","args":{"handler":handler,"value":{"close":true}}}});
        run.ack(&enter, &IntentOutcome::Applied { result });
        assert!(!crate::palette::active(&run.ui));
        assert!(run.session.tree.open_interaction().is_some());
        assert!(!run.ui.dialog.aside, "the ask owns the keyboard now");
        assert!(run.typeahead.is_empty());
        assert_eq!(session.submissions().len(), 1, "no key reached the ask");
        assert!(session.answers().is_empty(), "nothing answered the ask");
        assert_eq!(
            run.ui.notice().map(|notice| notice.text.as_str()),
            Some(typeahead::kept_out_notice(2).as_str())
        );
    }
}

#[tokio::test]
async fn keys_wait_no_longer_than_the_command_wait() {
    let now = still(scene().1);
    let (_home, mut run, session) = palette_run(now);
    typeahead::command_sent(&mut run, IntentId::from_raw("slow"));
    type_burst(&mut run, "hi");
    assert_eq!(run.typeahead.len(), 2);
    assert!(
        run.next_draw().is_some(),
        "the loop wakes when the wait ends"
    );
    run.typeahead.expire();
    typeahead::drain(&mut run);
    assert_eq!(run.ui.composer.text(), "hi");
    assert!(run.behind, "released keys owe a drawing");
    assert!(session.submitted().is_empty());
}

#[tokio::test]
async fn escape_ends_a_busy_command_hold_immediately() {
    interrupt_command_hold(key(KeyCode::Esc));
}

#[tokio::test]
async fn ctrl_c_ends_a_busy_command_hold_immediately() {
    interrupt_command_hold(ctrl('c'));
}

fn interrupt_command_hold(chord: crossterm::event::KeyEvent) {
    let now = still(scene().1);
    let (_home, mut run, session) = palette_run(now);
    run.session.tree.fold.apply(&frame(1, started("busy")));
    typeahead::command_sent(&mut run, IntentId::from_raw("slow"));
    type_burst(&mut run, "zq");
    run.terminal_event(Term::Key(chord));
    assert_eq!(
        session.interrupts(),
        1,
        "interrupt before any acknowledgement"
    );
    assert!(run.typeahead.is_empty());
    assert!(run.typeahead.deadline().is_none());
    assert!(run.ui.composer.is_empty());
}

#[tokio::test]
async fn deadline_release_owes_a_frame_showing_the_keys() {
    let now = still(scene().1);
    let (_home, mut run, _) = palette_run(now);
    let mut screen = crate::doubles::Recorder::default();
    typeahead::command_sent(&mut run, IntentId::from_raw("slow"));
    type_burst(&mut run, "zq");
    run.typeahead.expire();
    run.paint(&mut screen, Wake::Frame, now).unwrap();
    assert_eq!(run.ui.composer.text(), "zq");
    assert!(!screen.last().contains("zq"));
    assert!(run.next_draw().is_some(), "released keys owe a frame");
    run.paint(&mut screen, Wake::Frame, later(now, 100))
        .unwrap();
    assert!(screen.last().contains("zq"), "{}", screen.last());
}

#[tokio::test]
async fn pastes_wait_between_the_keys_they_were_typed_between() {
    let now = still(scene().1);
    let (_home, mut run, _) = palette_run(now);
    typeahead::command_sent(&mut run, IntentId::from_raw("slow"));
    type_burst(&mut run, "ab");
    run.terminal_event(Term::Paste("X".into()));
    type_burst(&mut run, "cd");
    run.typeahead.expire();
    typeahead::drain(&mut run);
    assert_eq!(run.ui.composer.text(), "abXcd");
    assert!(run.typeahead.is_empty());
}

#[tokio::test]
async fn queued_pastes_are_kept_out_of_asks() {
    let now = still(scene().1);
    let login = interaction(
        bingo_sdk::InteractionKind::Login {
            provider: "fixture".into(),
            flow: bingo_sdk::LoginFlow::Paste,
        },
        vec![bingo_sdk::AnswerSpec::Text, bingo_sdk::AnswerSpec::Cancel],
    );
    for ask in [permission(None, None), question(false, true), login] {
        for already_open in [false, true] {
            let (_home, mut run, session) = palette_run(now);
            if already_open {
                run.session.tree.fold.apply(&frame(1, opened(ask.clone())));
            }
            typeahead::command_sent(&mut run, IntentId::from_raw("slow"));
            run.terminal_event(Term::Paste("secret fragment".into()));
            if !already_open {
                run.session.tree.fold.apply(&frame(1, opened(ask.clone())));
            }
            run.typeahead.expire();
            typeahead::drain(&mut run);
            assert!(
                run.ui.composer.is_empty(),
                "a paste must not bypass the ask"
            );
            assert!(run.typeahead.is_empty());
            assert!(
                run.ui
                    .dialog
                    .words
                    .as_ref()
                    .is_none_or(|words| words.is_empty())
            );
            assert!(session.answers().is_empty());
            assert!(session.submitted().is_empty());
            assert_eq!(
                run.ui.notice().expect("a notice").text,
                typeahead::kept_out_notice(1)
            );
        }
    }
}

#[tokio::test]
async fn a_set_aside_ask_never_receives_keys_held_in_the_composer() {
    let now = still(scene().1);
    let (_home, mut run, session) = palette_run(now);
    run.session
        .tree
        .fold
        .apply(&frame(1, opened(permission(None, None))));
    run.ui.dialog.aside = true;
    typeahead::command_sent(&mut run, IntentId::from_raw("slow"));
    type_burst(&mut run, "y");
    run.ui.dialog.aside = false;
    run.typeahead.expire();
    typeahead::drain(&mut run);
    assert!(session.answers().is_empty());
    assert!(run.typeahead.is_empty());
    assert_eq!(
        run.ui.notice().expect("a notice").text,
        typeahead::kept_out_notice(1)
    );
}

#[tokio::test]
async fn an_unknown_command_never_answers_the_ask_it_set_aside() {
    let now = still(scene().1);
    for rendered in [false, true] {
        let (_home, mut run, session) = palette_run(now);
        if !rendered {
            run.session = Attached::new(state(), SessionHandle(session.clone()));
        }
        run.session
            .tree
            .fold
            .apply(&frame(1, opened(permission(None, None))));
        type_burst(&mut run, "/");
        assert!(run.ui.dialog.aside);
        type_burst(&mut run, "nosuch\ny");
        if rendered {
            play_palette(&mut run, &session, now);
        }
        run.typeahead.expire();
        typeahead::drain(&mut run);
        assert!(session.answers().is_empty(), "y was typed outside the ask");
        assert!(run.session.tree.open_interaction().is_some());
        assert!(run.typeahead.is_empty());
        assert_eq!(
            run.ui.notice().expect("a notice").text,
            typeahead::kept_out_notice(1)
        );
    }
}

fn status_id() -> &'static str {
    bingo_sdk::Plugin::manifest(&bingo_mod_status::Status::default()).id
}

fn status_pane(state: &mut SessionState) {
    let rows = bingo_mod_status::rows::rows(&(&*state).into(), &[], &[], None);
    let view = bingo_mod_status::draw::tree(&rows, "session", 108, 12, 0);
    let pane: bingo_sdk::slot::Slot = serde_json::from_value(json!({
        "kind":"pane", "id":"status", "title":"", "focus":true, "asked":true,
        "closeOnEscape":true, "holdToasts":true, "rows":12, "view":view
    }))
    .unwrap();
    state.signals.entry(status_id().into()).or_default().insert(
        "status".into(),
        bingo_sdk::slot::with(json!({}), [pane]).into(),
    );
}

#[tokio::test]
async fn held_status_keys_follow_the_pane_handoff_but_never_a_returning_ask() {
    let now = still(scene().1);
    for ask in [false, true] {
        let mut state = state();
        status_pane(&mut state);
        if ask {
            state.apply(&frame(1, opened(permission(None, None))));
        }
        let session = Arc::new(TestSession::default());
        let mut run = tests::idle_in(state, session.clone(), now.instant);
        run.ui.dialog.aside = ask;
        let mut screen = crate::doubles::Recorder::default();
        run.paint(&mut screen, Wake::Frame, now).unwrap();
        assert_eq!(
            crate::focus::owner(&run.ui, &run.session.tree),
            crate::focus::Owner::Pane
        );
        type_burst(&mut run, "\ny");
        let (intent, action) = unanswered(&session, &[]).unwrap();
        assert_eq!(action.name, format!("{}:ui", status_id()));
        assert_eq!(action.args["value"]["key"], "enter");
        assert_eq!(run.typeahead.len(), 1);
        if ask {
            run.ui.dialog.aside = false;
        }
        run.session.tree.fold.apply(&frame(
            2,
            Event::Signal {
                generation: None,
                plugin: status_id().into(),
                kind: "status".into(),
                payload: json!({}),
            },
        ));
        run.paint(&mut screen, Wake::Frame, now).unwrap();
        run.ack(
            &intent,
            &IntentOutcome::Applied {
                result: json!({"handled":true}),
            },
        );
        assert!(run.typeahead.is_empty());
        assert!(session.answers().is_empty());
        assert_eq!(
            session.submissions().len(),
            1,
            "no held key is sent back to the old pane"
        );
        assert_eq!(run.ui.composer.text(), if ask { "" } else { "y" });
        if ask {
            assert_eq!(
                run.ui.notice().expect("a notice").text,
                typeahead::kept_out_notice(1)
            );
        }
    }
}

#[tokio::test]
async fn a_surface_run_line_holds_keys_until_its_status_pane_opens() {
    let now = still(scene().1);
    let (_home, mut run, session) = palette_run(now);
    run.effect(Effect::Run("/status".into()));
    type_burst(&mut run, "xy");
    assert_eq!(run.typeahead.len(), 2);
    let (intent, input) = session.submissions()[0].clone();
    assert!(matches!(input, Input::Text { text, .. } if text == "/status"));
    let mut state = state();
    status_pane(&mut state);
    run.session.tree.fold.apply(&frame(
        1,
        Event::Signal {
            generation: None,
            plugin: status_id().into(),
            kind: "status".into(),
            payload: state.signals[status_id()]["status"].payload().clone(),
        },
    ));
    let mut screen = crate::doubles::Recorder::default();
    run.paint(&mut screen, Wake::Frame, now).unwrap();
    run.ack(
        &intent,
        &IntentOutcome::Applied {
            result: json!({"surface":{"name":"Pane","args":{"plugin":status_id(),"id":"status"}}}),
        },
    );
    let (key_intent, action) = unanswered(&session, &[]).unwrap();
    assert_eq!(action.name, format!("{}:ui", status_id()));
    assert_eq!(action.args["value"]["key"], "x");
    run.ack(
        &key_intent,
        &IntentOutcome::Applied {
            result: json!({"handled":false}),
        },
    );
    assert_eq!(run.ui.composer.text(), "xy");
    assert!(run.typeahead.is_empty());
}

fn publish_status(run: &mut Run, pulled: bool) -> View {
    let mut state = state();
    status_pane(&mut state);
    let mut payload = state.signals[status_id()]["status"].payload().clone();
    let view = serde_json::from_value(payload["slots"][0]["view"].clone()).unwrap();
    if pulled {
        payload["slots"][0]["view"] = json!({"request":{
            "component":"Status", "requestId":"status", "props":{},
            "viewport":{"columns":0,"rows":0}
        }});
        state.config.kernel["render"] = json!({"Status":[{}]});
        run.session.tree.fold.apply(&frame(
            1,
            Event::ConfigChanged {
                config: state.config,
            },
        ));
    }
    run.session.tree.fold.apply(&frame(
        2,
        Event::Signal {
            generation: None,
            plugin: status_id().into(),
            kind: "status".into(),
            payload,
        },
    ));
    view
}

fn answer_status_drawing(run: &mut Run, view: View) {
    let key = run
        .ui
        .shown
        .pulls
        .iter()
        .find(|key| key.request.component == Component::Status)
        .unwrap()
        .clone();
    run.ui.placed.pulled.request(&key);
    run.reply(
        Reply::Rendered(run.ui.placed.pulled.epoch(), key, Box::new(view)),
        &mut None,
    );
}

#[tokio::test]
async fn status_receipt_before_paint_keeps_keys_until_the_pane_can_read_them() {
    let now = still(scene().1);
    for pulled in [false, true] {
        let (_home, mut run, session) = palette_run(now);
        run.effect(Effect::Run("/status".into()));
        type_burst(&mut run, "x");
        let intent = session.submissions()[0].0.clone();
        let view = publish_status(&mut run, pulled);
        run.ack(&intent, &IntentOutcome::Applied {
            result: json!({"surface":{"name":"Pane","args":{"plugin":status_id(),"id":"status"}}}),
        });
        assert_eq!(
            run.typeahead.len(),
            1,
            "the receipt precedes the pane's first drawing"
        );
        draw_tree(108, 30, &run.session.tree, &mut run.ui, now);
        typeahead::drain(&mut run);
        if pulled {
            assert_eq!(
                run.typeahead.len(),
                1,
                "the pane is seated but its view is still out"
            );
            answer_status_drawing(&mut run, view);
        }
        let (key_intent, action) = unanswered(&session, &[]).unwrap();
        assert_eq!(action.name, format!("{}:ui", status_id()));
        assert_eq!(action.args["value"]["key"], "x");
        run.ack(
            &key_intent,
            &IntentOutcome::Applied {
                result: json!({"handled":false}),
            },
        );
        assert_eq!(run.ui.composer.text(), "x");
        assert!(run.typeahead.is_empty());
    }
}

#[tokio::test]
async fn interrupts_do_not_wait_for_a_status_panes_first_drawing() {
    let now = still(scene().1);
    for chord in [key(KeyCode::Esc), ctrl('c')] {
        let (_home, mut run, session) = palette_run(now);
        publish_status(&mut run, true);
        run.session.tree.fold.apply(&frame(3, started("busy")));
        surface::action(
            &mut run,
            Action {
                name: "Pane".into(),
                args: json!({"plugin":status_id(),"id":"status"}),
            },
        );
        typeahead::command_sent(&mut run, IntentId::from_raw("slow"));
        type_burst(&mut run, "zq");
        run.terminal_event(Term::Key(chord));
        assert_eq!(session.interrupts(), 1);
        assert!(run.typeahead.is_empty());
        assert!(run.typeahead.deadline().is_none());
    }
}

#[tokio::test]
async fn query_edits_echo_without_a_palette_answer_but_enter_waits() {
    let now = still(scene().1);
    let (_home, mut run, session) = palette_run(now);
    type_burst(&mut run, "/permission");
    assert_eq!(run.ui.composer.text(), "/permission");
    assert!(run.typeahead.is_empty());
    run.terminal_event(Term::Key(key(KeyCode::Left)));
    run.terminal_event(Term::Key(key(KeyCode::Backspace)));
    assert_eq!(run.ui.composer.text(), "/permissin");
    let picture = draw_tree(80, 24, &run.session.tree, &mut run.ui, now);
    assert!(picture.contains("/permissin"));
    run.terminal_event(Term::Key(key(KeyCode::Enter)));
    type_burst(&mut run, "z");
    assert_eq!(run.ui.composer.text(), "/permissin");
    assert_eq!(run.typeahead.len(), 2);
    assert!(session.submitted().is_empty());
    play_palette(&mut run, &session, now);
    assert_eq!(texts(&session), ["/permissin"]);
}

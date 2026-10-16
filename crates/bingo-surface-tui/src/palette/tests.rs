use super::*;
use crate::test_support::key;
use crate::test_support::*;
use bingo_sdk::{Contribution, HookContext, Next, Plugin, Registrar};
use crossterm::event::KeyModifiers;
use serde_json::Value;
use std::sync::Arc;

pub(crate) struct Fixture {
    _home: tempfile::TempDir,
    link: Arc<dyn Interceptor>,
    pub(crate) handler: Arc<dyn Command>,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut registrar = Registrar::new(
            "palette",
            json!({}),
            Env::rooted(home.path()),
            Default::default(),
        );
        bingo_mod_palette::Palette.register(&mut registrar).unwrap();
        let contributions = registrar.into_contributions();
        Self {
            _home: home,
            link: contributions
                .iter()
                .find_map(|c| {
                    if let Contribution::Interceptor { link, .. } = c {
                        Some(link.clone())
                    } else {
                        None
                    }
                })
                .unwrap(),
            handler: contributions
                .iter()
                .find_map(|c| {
                    if let Contribution::Command(command) = c
                        && command.spec().hidden
                    {
                        Some(command.clone())
                    } else {
                        None
                    }
                })
                .unwrap(),
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
        self.paint_tree(ui, &solo(state), now, width, height).await
    }
    pub(crate) async fn paint_tree(
        &self,
        ui: &mut Ui,
        tree: &Tree,
        now: Now,
        width: u16,
        height: u16,
    ) -> String {
        let state = tree.viewed();
        ui.catalogs
            .current
            .get_or_insert_with(|| Current(Arc::new(Values)));
        draw_tree(width, height, tree, ui, now);
        let key = ui
            .shown
            .pulls
            .iter()
            .find(|key| key.request.component == render::Component::Palette)
            .unwrap()
            .clone();
        let host = testing::TestHost::default();
        host.on_service_any(|key| {
            if key != command::COMMAND_CATALOG.name() {
                return None;
            }
            let reader: Arc<dyn command::CommandCatalog> = Arc::new(Values);
            Some(Arc::new(reader))
        });
        let cx = HookContext {
            session: state.summary.id.clone(),
            turn: None,
            item: None,
            cwd: state.summary.cwd.clone().into(),
            provider: None,
            model: None,
            host: host.handle(),
        };
        let value = self
            .link
            .chain(
                catalogue::UI_RENDER,
                &cx,
                serde_json::to_value(&key.request).unwrap(),
                Next::new(|value| Box::pin(async move { Ok(value) })),
            )
            .await
            .unwrap();
        let view: View = serde_json::from_value(value).unwrap();
        assert!(
            view::answer_ok(
                render::Component::Palette,
                &key.request.props,
                "palette",
                None,
                &view
            )
            .is_ok()
        );
        ui.placed.pulled.request(&key);
        ui.placed.pulled.answer(key, view);
        draw_tree(width, height, tree, ui, now)
    }
    async fn press(
        &self,
        ui: &mut Ui,
        state: &SessionState,
        key: KeyEvent,
        now: Now,
    ) -> Vec<Effect> {
        self.press_tree(ui, &solo(state), key, now).await
    }
    pub(crate) async fn press_tree(
        &self,
        ui: &mut Ui,
        tree: &Tree,
        key: KeyEvent,
        now: Now,
    ) -> Vec<Effect> {
        let state = tree.viewed();
        let effects = crate::input::on_key(ui, tree, key, now);
        let Some(Effect::SiteKey { action, .. }) = effects.first() else {
            return effects;
        };
        let cx = CommandContext {
            session: state.summary.id.clone(),
            cwd: state.summary.cwd.clone().into(),
            host: testing::NoHost::handle(),
            cancel: Default::default(),
        };
        match self
            .handler
            .run(&action.args.to_string(), &cx)
            .await
            .unwrap()
        {
            CommandOutcome::Surface { action, .. } => result(ui, tree, &action, true, now),
            CommandOutcome::Handled { handled: false } => {
                crate::input::palette_edit(ui, tree, key, now)
            }
            _ => vec![],
        }
    }
}
fn setup() -> (SessionState, Ui, Now) {
    let mut state = state();
    state.config.kernel["render"] = json!({"Palette":[{}]});
    state.config.kernel["policy"] = json!("policy");
    state.config.plugins.insert(
        "policy".into(),
        slot::with(
            json!({}),
            [slot::Slot::Mode {
                current: "default".into(),
                modes: vec![ActionItem {
                    label: "default".into(),
                    action: Action {
                        name: "permission".into(),
                        args: json!("default"),
                    },
                    key: None,
                }],
            }],
        ),
    );
    let (mut ui, now) = scene();
    ui.catalogs.commands=crate::commands::entries([("model",json!({"kind":"catalog","source":"models"})),("think",json!({"kind":"words","values":["off","low","high"]})),("rename",json!({"kind":"free","hint":"a name for this session"})),("permission",json!({"kind":"words","values":["default","acceptEdits","plan"]})),("resume",json!({"kind":"none"})),("login",json!({"kind":"catalog","source":"providers"}))].into_iter().map(|(name,args)|serde_json::from_value(json!({"name":name,"hint":format!("choose {name}"),"args":args,"family":"session","keys":if name=="resume"{vec!["ctrl+r"]}else{vec![]}})).unwrap()).collect());
    ui.catalogs.values.insert(
        "models".into(),
        ["fake/small", "fake/large"]
            .map(|id| CatalogEntry {
                id: id.into(),
                label: id.into(),
                meta: Value::Null,
            })
            .to_vec(),
    );
    ui.catalogs.values.insert(
        "providers".into(),
        [
            ("anthropic", json!({"kind": "ready"})),
            (
                "openai",
                json!({"kind": "missing", "hint": "set OPENAI_API_KEY"}),
            ),
            ("fake", json!({"kind": "notApplicable"})),
        ]
        .map(|(id, auth)| CatalogEntry {
            id: id.into(),
            label: id.into(),
            meta: json!({ "auth": auth }),
        })
        .to_vec(),
    );
    (state, ui, still(now))
}
fn sessions(ui: &mut Ui, state: &SessionState) {
    let Some(Open::Palette(open)) = ui.focus.open_mut() else {
        panic!("palette");
    };
    let mut other = state.summary.clone();
    other.id = SessionId::from_raw("ses_other");
    other.title = Some("model migration".into());
    other.history.messages = Some(12);
    open.sessions = vec![state.summary.clone(), other];
}
#[tokio::test]
async fn prototype_scenarios_at_both_sizes() {
    for (width, height) in [(108, 30), (80, 24)] {
        for (name, first, query) in [
            ("commands", PaletteOpen::Commands, "/"),
            ("keys", PaletteOpen::Keys, ""),
            ("sessions", PaletteOpen::Sessions, ""),
            ("mo", PaletteOpen::Commands, "/mo"),
            ("think", PaletteOpen::Commands, "/think "),
            ("rename", PaletteOpen::Commands, "/rename release notes"),
            ("permission", PaletteOpen::Commands, "/permission "),
            ("resume", PaletteOpen::Commands, "/resume "),
            ("login", PaletteOpen::Commands, "/login "),
        ] {
            let fixture = Fixture::new();
            let (state, mut ui, now) = setup();
            open(&mut ui, first, now);
            sessions(&mut ui, &state);
            ui.composer.set(query);
            let screen = fixture.paint(&mut ui, &state, now, width, height).await;
            insta::assert_snapshot!(format!("palette_{name}_{width}x{height}"), screen);
        }
    }
}
/// `count` skills on the shelves a person has, a plugin's guide among the
/// first three.
fn skills(ui: &mut Ui, count: usize) {
    const NAMED: [(&str, &str, &str); 3] = [
        ("deploy", "ship the current branch to staging", "project"),
        ("standup", "write today's standup from the git log", "yours"),
        ("guide-memory", "how memory is kept and recalled", "guide"),
    ];
    let skills = (0..count).map(|n| match NAMED.get(n) {
        Some((name, hint, shelf)) => ((*name).to_owned(), (*hint).to_owned(), *shelf),
        None => (
            format!("docs-{n:02}"),
            format!("write the docs page number {n} from its outline"),
            "project",
        ),
    });
    ui.catalogs.commands.extend(
        skills
            .map(|(name, hint, shelf)| {
                serde_json::from_value::<bingo_sdk::CommandSpec>(json!({
                    "name": name,
                    "hint": hint,
                    "args": {"kind": "free", "hint": ""},
                    "family": format!("skill/{shelf}"),
                }))
                .unwrap()
            })
            .map(crate::commands::entry),
    );
}
#[tokio::test]
async fn the_skills_group_at_zero_three_and_twenty() {
    for (width, height) in [(108, 30), (80, 24)] {
        for count in [0, 3, 20] {
            for (name, query) in [("bare", "/"), ("search", "/d")] {
                let fixture = Fixture::new();
                let (state, mut ui, now) = setup();
                skills(&mut ui, count);
                open(&mut ui, PaletteOpen::Commands, now);
                ui.composer.set(query);
                let screen = fixture.paint(&mut ui, &state, now, width, height).await;
                let reachable = count == 0 || screen.contains("skill");
                assert!(reachable, "every skill stays reachable from /: {screen}");
                insta::assert_snapshot!(
                    format!("palette_skills_{count}_{name}_{width}x{height}"),
                    screen
                );
            }
        }
    }
}
#[tokio::test]
async fn a_query_nothing_matches_says_so() {
    let fixture = Fixture::new();
    let (state, mut ui, now) = setup();
    open(&mut ui, PaletteOpen::Commands, now);
    ui.composer.set("/zz");
    let screen = fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert!(screen.contains("nothing matches “zz”"), "{screen}");
    insta::assert_snapshot!("palette_nothing_matches_80x24", screen);
}
#[tokio::test]
async fn search_arguments_escape_and_resume_use_the_same_site() {
    let fixture = Fixture::new();
    let (state, mut ui, now) = setup();
    assert!(matches!(
        crate::input::on_key(&mut ui, &solo(&state), typed('/'), now).as_slice(),
        [Effect::ListStored]
    ));
    sessions(&mut ui, &state);
    ui.composer.set("/mo");
    let screen = fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert!(
        screen.contains("model migration")
            && screen.contains("commands")
            && screen.contains("sessions"),
        "{screen}"
    );
    fixture
        .press(&mut ui, &state, key(KeyCode::Enter), now)
        .await;
    assert_eq!(ui.composer.text(), "/model ");
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    fixture.press(&mut ui, &state, key(KeyCode::Esc), now).await;
    assert_eq!(ui.composer.text(), "/");
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    fixture.press(&mut ui, &state, key(KeyCode::Esc), now).await;
    assert!(!active(&ui));
    open(&mut ui, PaletteOpen::Sessions, now);
    sessions(&mut ui, &state);
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    fixture
        .press(&mut ui, &state, key(KeyCode::Down), now)
        .await;
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    let effects = fixture
        .press(&mut ui, &state, key(KeyCode::Enter), now)
        .await;
    assert!(
        matches!(&effects[0],Effect::Open(SessionSelector::ById{id}) if id.as_str()=="ses_other")
    );
}
#[test]
fn help_resume_and_shift_tab_keep_their_own_routes() {
    let (state, mut ui, now) = setup();
    let tree = solo(&state);
    ui.catalogs
        .commands
        .push(crate::commands::entry(CommandSpec {
            opens: false,
            name: "help".into(),
            ..crate::commands::local_specs()[0].clone()
        }));
    for (key, line) in [(typed('?'), "/help"), (ctrl('r'), "/resume")] {
        let effects = crate::input::on_key(&mut ui, &tree, key, now);
        assert_eq!(effects[0], Effect::Run(line.into()));
    }
    crate::input::on_key(
        &mut ui,
        &tree,
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        now,
    );
    assert!(!active(&ui));
}
#[test]
fn disabled_palette_lists_names_and_runs_typed_commands() {
    let (mut state, mut ui, now) = setup();
    state.config.kernel["render"] = json!({});
    let tree = solo(&state);
    crate::input::on_key(&mut ui, &tree, typed('/'), now);
    let screen = draw_sized(80, 24, &state, &mut ui, now);
    assert!(screen.contains("/model"));
    assert!(!screen.contains("choose model"));
    ui.composer.set("/rename explicit");
    let effects = crate::input::on_key(&mut ui, &tree, key(KeyCode::Enter), now);
    assert!(matches!(&effects[0],Effect::Submit(Input::Text{text,..}) if text=="/rename explicit"));
}
#[tokio::test]
async fn stale_query_and_engine_answers_cannot_trap_the_keyboard() {
    let fixture = Fixture::new();
    let (state, mut ui, now) = setup();
    open(&mut ui, PaletteOpen::Commands, now);
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    ui.composer.set("/rename ");
    assert!(waiting(&ui, &solo(&state), now));
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert!(!waiting(&ui, &solo(&state), now));
    let key = ui
        .shown
        .pulls
        .iter()
        .find(|key| key.request.component == render::Component::Palette)
        .unwrap()
        .clone();
    ui.placed.pulled.answer(key.clone(), key.request.engine());
    assert!(!waiting(&ui, &solo(&state), now));
}

struct Values;
impl command::CommandCatalog for Values {
    fn catalog(&self, mut catalog: Catalog, view: &command::SessionView<'_>) -> Catalog {
        if catalog.entries.iter().any(|entry| entry.id == "permission") {
            assert!(slot::placed(view.state).iter().any(
            |placed| matches!(&placed.slot, slot::Slot::Mode {current, ..} if current=="default")
        ));
        }
        for entry in &mut catalog.entries {
            entry.meta["now"] = json!(match entry.id.as_str() {
                "model" => "fake/small",
                "think" => "high",
                "rename" => "Release notes",
                "permission" => "default",
                _ => "",
            });
        }
        catalog
    }
}

#[tokio::test]
async fn tab_does_not_run_arguments_and_escape_leaves_free_text_at_once() {
    let fixture = Fixture::new();
    let (state, mut ui, now) = setup();
    open(&mut ui, PaletteOpen::Commands, now);
    ui.composer.set("/think ");
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert!(
        fixture
            .press(&mut ui, &state, key(KeyCode::Tab), now)
            .await
            .is_empty()
    );
    assert_eq!(ui.composer.text(), "/think ");
    let effects = fixture
        .press(&mut ui, &state, key(KeyCode::Enter), now)
        .await;
    assert!(matches!(&effects[0], Effect::Submit(Input::Text {text,..}) if text=="/think high"));
    assert_eq!(
        ui.history.older("").as_deref(),
        Some("/think high"),
        "a line typed into the palette is the person's own"
    );
    ui.history.reset();
    open(&mut ui, PaletteOpen::Commands, now);
    ui.composer.set("/rename several words");
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    fixture.press(&mut ui, &state, key(KeyCode::Esc), now).await;
    assert_eq!(ui.composer.text(), "/");
}

#[tokio::test]
async fn choosing_the_mode_key_replays_shift_tab_after_closing() {
    let fixture = Fixture::new();
    let (state, mut ui, now) = setup();
    open(&mut ui, PaletteOpen::Keys, now);
    fixture.paint(&mut ui, &state, now, 80, 24).await;
    let effects = fixture
        .press(&mut ui, &state, key(KeyCode::Enter), now)
        .await;
    assert!(!active(&ui));
    assert!(matches!(&effects[0], Effect::Run(line) if line.starts_with("/permission ")));
}

#[test]
fn ctrl_p_is_unbound_and_absent_from_the_palette_keys() {
    let (state, mut ui, now) = setup();
    assert!(crate::input::on_key(&mut ui, &solo(&state), ctrl('p'), now).is_empty());
    assert!(!active(&ui));
    open(&mut ui, PaletteOpen::Keys, now);
    draw_sized(80, 24, &state, &mut ui, now);
    let props = &ui
        .shown
        .pulls
        .iter()
        .find(|key| key.request.component == render::Component::Palette)
        .unwrap()
        .request
        .props;
    assert!(props["keyHints"].get("ctrl+p").is_none());
    assert!(props["bindings"].get("ctrl+p").is_none());
    let escape = crate::keys::BINDINGS
        .iter()
        .find(|binding| binding.keys == "esc")
        .unwrap();
    assert_eq!(
        escape.description,
        crate::keys::ESCAPES
            .iter()
            .map(|(_, word)| *word)
            .collect::<Vec<_>>()
            .join(" → ")
    );
}

#[test]
fn disabled_palette_keeps_help_and_resume_in_the_client() {
    let (mut state, mut ui, now) = setup();
    state.config.kernel =
        json!({"enabledPlugins":{bingo_mod_palette::Palette.manifest().id:false}});
    ui.catalogs.commands.clear();
    let tree = solo(&state);
    for opener in [typed('?'), ctrl('r')] {
        assert_eq!(
            crate::input::on_key(&mut ui, &tree, opener, now),
            vec![Effect::ListStored]
        );
        assert!(active(&ui));
        close(&mut ui, now);
    }
    assert_eq!(
        crate::input::command_line(&mut ui, &tree, now, "/resume".into()),
        vec![Effect::ListStored]
    );
    sessions(&mut ui, &state);
    let effects = super::key(&mut ui, &tree, key(KeyCode::Down), now);
    assert!(effects.is_empty());
    assert_eq!(
        super::key(&mut ui, &tree, key(KeyCode::Enter), now),
        vec![Effect::Open(SessionSelector::ById {
            id: SessionId::from_raw("ses_other")
        })]
    );
}

/// The palette is a site too: a trust line from an answer the kernel did not
/// vouch for neither runs nor is sent by a key it replays — closing gives
/// the composer back its draft, which a replayed `⏎` would send.
#[test]
fn a_trust_line_nobody_vouched_for_stays_unsent() {
    let submits = |effects: &[Effect]| effects.iter().any(|e| matches!(e, Effect::Submit(_)));
    for vouched in [false, true] {
        for value in [
            json!({"run":"/plugins trust demo full"}),
            json!({"key":"enter"}),
        ] {
            let (state, mut ui, now) = setup();
            ui.composer.set("/mcp approve local");
            open(&mut ui, PaletteOpen::Keys, now);
            let action = Action {
                name: "surface:palette".into(),
                args: json!({"handler":request(&ui).unwrap(),"value":value}),
            };
            let effects = result(&mut ui, &solo(&state), &action, vouched, now);
            assert_eq!(submits(&effects), vouched, "{value}");
            let said = ui
                .notice()
                .is_some_and(|notice| notice.text.contains("type it yourself"));
            assert_eq!(said, !vouched, "{value}");
        }
    }
}

#[tokio::test]
async fn pending_queries_keep_the_accepted_drawing_and_its_height() {
    let fixture = Fixture::new();
    let (state, mut ui, now) = setup();
    let tree = solo(&state);
    open(&mut ui, PaletteOpen::Commands, now);
    let first = fixture.paint(&mut ui, &state, now, 80, 24).await;
    let header = first
        .lines()
        .position(|line| line.trim_matches('"').trim() == "commands")
        .unwrap();
    for letter in "rename".chars() {
        crate::input::on_key(&mut ui, &tree, typed(letter), now);
        let pending = draw_tree(80, 24, &tree, &mut ui, now);
        assert_eq!(pending.lines().nth(header), first.lines().nth(header));
        assert!(
            pending.contains("choose model"),
            "last drawing disappeared: {pending}"
        );
        assert!(waiting(&ui, &tree, now), "old rows must not accept actions");
    }
    let current = fixture.paint(&mut ui, &state, now, 80, 24).await;
    assert_eq!(
        current
            .lines()
            .position(|line| line.trim_matches('"').trim() == "commands"),
        Some(header)
    );
    assert!(!waiting(&ui, &tree, now));
    let moved = crate::input::on_key(&mut ui, &tree, key(KeyCode::Left), now);
    assert!(moved.is_empty());
    assert_eq!(draw_tree(80, 24, &tree, &mut ui, now), current);
}

#[tokio::test]
async fn bare_permission_and_rename_enter_draw_their_inline_arguments() {
    let fixture = Fixture::new();
    let (state, mut ui, now) = setup();
    for (name, offered) in [
        ("permission", "acceptEdits"),
        ("rename", "a name for this session"),
    ] {
        open(&mut ui, PaletteOpen::Commands, now);
        ui.composer.set(&format!("/{name}"));
        fixture.paint(&mut ui, &state, now, 80, 24).await;
        let effects = fixture
            .press(&mut ui, &state, key(KeyCode::Enter), now)
            .await;
        assert!(effects.is_empty());
        assert_eq!(ui.composer.text(), format!("/{name} "));
        let screen = fixture.paint(&mut ui, &state, now, 80, 24).await;
        assert!(screen.contains(offered), "{screen}");
        if name == "permission" {
            assert!(screen.contains("now"), "{screen}");
        }
        close(&mut ui, now);
    }
}

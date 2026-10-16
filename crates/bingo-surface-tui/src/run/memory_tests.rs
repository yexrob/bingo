use super::*;
use crate::{doubles::TestSession, test_support::*};
use bingo_sdk::{
    Command, CommandContext, CommandOutcome, Contribution, Env, HookContext, Interceptor, Next,
    Plugin, Registrar, catalogue, render, testing::TestHost,
};
use crossterm::event::KeyCode;
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

struct MemoryFixture {
    home: tempfile::TempDir,
    env: Env,
    host: TestHost,
    signals: Arc<Mutex<Vec<Event>>>,
    open: Arc<dyn Interceptor>,
    draw: Arc<dyn Interceptor>,
    handler: Arc<dyn Command>,
    session: Arc<TestSession>,
    run: Run,
}
fn owners(env: &Env) -> TestHost {
    let mut services = HashMap::new();
    for plugin in [
        Box::new(bingo_context::ContextPlugin) as Box<dyn Plugin>,
        Box::new(bingo_experience::ExperiencePlugin::default()),
    ] {
        let mut registrar = Registrar::new(
            plugin.manifest().id,
            json!({}),
            env.clone(),
            Default::default(),
        );
        plugin.register(&mut registrar).unwrap();
        for contribution in registrar.into_contributions() {
            if let Contribution::Service { key, value, .. } = contribution {
                services.insert(key, value);
            }
        }
    }
    let host = TestHost::default();
    host.on_service_any(move |key| services.get(&key).cloned());
    host
}
impl MemoryFixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let env = Env::rooted(home.path());
        let host = owners(&env);
        let signals = Arc::new(Mutex::new(Vec::new()));
        let collected = signals.clone();
        host.on_signal(move |_, plugin, kind, payload| {
            collected.lock().unwrap().push(Event::Signal {
                generation: None,
                plugin,
                kind,
                payload,
            });
            async { Ok(()) }
        });
        let mut registrar = Registrar::new(
            bingo_mod_memory::MemoryPanel.manifest().id,
            json!({}),
            env.clone(),
            Default::default(),
        );
        bingo_mod_memory::MemoryPanel
            .register(&mut registrar)
            .unwrap();
        let contributions = registrar.into_contributions();
        let link = |id| {
            contributions
                .iter()
                .find_map(|c| match c {
                    Contribution::Interceptor { link, .. } if link.id() == id => Some(link.clone()),
                    _ => None,
                })
                .unwrap()
        };
        let handler = contributions
            .iter()
            .find_map(|c| match c {
                Contribution::Command(command) => Some(command.clone()),
                _ => None,
            })
            .unwrap();
        let mut state = state();
        state.summary.cwd = home.path().display().to_string();
        state.config.kernel["render"] = json!({"Pane":[{}]});
        let session = Arc::new(TestSession::default());
        let mut run = tests::idle_in(state, session.clone(), still(scene().1).instant);
        run.host = host.handle();
        Self {
            home,
            env,
            host,
            signals,
            open: link("memory:open"),
            draw: link(bingo_mod_memory::ID),
            handler,
            session,
            run,
        }
    }
    fn cx(&self) -> HookContext {
        HookContext {
            session: self.run.session.tree.view().clone(),
            turn: None,
            item: None,
            cwd: self.home.path().into(),
            provider: None,
            model: None,
            host: self.host.handle(),
        }
    }
    fn fold(&mut self) {
        for event in self.signals.lock().unwrap().drain(..) {
            let seq = self.run.session.tree.root().seq.next().0;
            self.run.session.tree.fold.apply(&frame(seq, event));
        }
    }
    async fn open(&mut self) {
        let outcome = self
            .open
            .chain(
                catalogue::COMMAND_RUN,
                &self.cx(),
                json!({"command":"memory","args":"","surface":"terminal"}),
                Next::new(|_| {
                    Box::pin(async { Ok(json!({"kind":"applied","message":"fallback"})) })
                }),
            )
            .await
            .unwrap();
        self.fold();
        let CommandOutcome::Surface { action, fallback } = serde_json::from_value(outcome).unwrap()
        else {
            panic!("surface")
        };
        assert_eq!(fallback.as_deref(), Some("fallback"));
        surface::action(&mut self.run, action);
    }
    fn seed(&self) -> std::path::PathBuf {
        let dir = self.env.data_dir.join("memory/user");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rule.md"), "---\nname: rule\ntype: feedback\ndescription: Use fixtures\n---\n\nRead this full memory.\n").unwrap();
        std::fs::write(dir.join("MEMORY.md"), "# Memory\n[rule](rule.md)\nkeep\n").unwrap();
        std::fs::create_dir_all(&self.env.config_dir).unwrap();
        std::fs::write(
            self.env.config_dir.join("AGENTS.md"),
            "Use fixture contracts.",
        )
        .unwrap();
        dir
    }
    async fn paint(&mut self, width: u16, height: u16) -> String {
        self.fold();
        let now = still(scene().1);
        draw_tree(width, height, &self.run.session.tree, &mut self.run.ui, now);
        let key = self
            .run
            .ui
            .shown
            .pulls
            .iter()
            .find(|key| key.request.component == render::Component::Pane)
            .unwrap()
            .clone();
        let value = self
            .draw
            .chain(
                catalogue::UI_RENDER,
                &self.cx(),
                serde_json::to_value(&key.request).unwrap(),
                Next::new(|v| Box::pin(async { Ok(v) })),
            )
            .await
            .unwrap();
        let view: View = serde_json::from_value(value).unwrap();
        bingo_sdk::view::answer_ok(
            render::Component::Pane,
            &key.request.props,
            bingo_mod_memory::ID,
            None,
            &view,
        )
        .unwrap();
        self.run.ui.placed.pulled.request(&key);
        self.run.ui.placed.pulled.answer(key, view);
        draw_tree(width, height, &self.run.session.tree, &mut self.run.ui, now)
    }
    async fn press(&mut self, key: crossterm::event::KeyEvent) {
        let count = self.session.submissions().len();
        self.run.terminal_event(Term::Key(key));
        assert_eq!(
            self.session.submissions().len(),
            count + 1,
            "a fresh key action"
        );
        let submissions = self.session.submissions();
        let (intent, Input::Action { action, .. }) = submissions.last().unwrap() else {
            panic!("pane action")
        };
        let cx = self.cx();
        let outcome = self
            .handler
            .run(
                &action.args.to_string(),
                &CommandContext {
                    session: cx.session,
                    cwd: cx.cwd,
                    host: cx.host,
                    cancel: Default::default(),
                },
            )
            .await
            .unwrap();
        self.fold();
        let result = match outcome {
            CommandOutcome::Handled { handled } => json!({"handled":handled}),
            CommandOutcome::Applied { message } => json!({"message":message}),
            CommandOutcome::Surface { action, .. } => json!({"surface":action}),
            _ => panic!("panel receipt"),
        };
        self.run.ack(intent, &IntentOutcome::Applied { result });
    }
}
#[tokio::test]
async fn memory_panel_reads_forgets_and_refuses_instruction_deletion() {
    let mut fixture = MemoryFixture::new();
    let dir = fixture.seed();
    fixture.open().await;
    assert!(fixture.paint(108, 30).await.contains("Use fixtures"));
    fixture.press(key(KeyCode::Enter)).await;
    assert!(matches!(fixture.run.ui.focus.open(), Open::Pager(_)));
    let screen = draw_tree(
        108,
        30,
        &fixture.run.session.tree,
        &mut fixture.run.ui,
        still(scene().1),
    );
    assert!(screen.contains("Read this full memory."), "{screen}");
    assert!(
        screen
            .lines()
            .any(|line| line.trim_matches('"').trim() == "---"),
        "the panel fences its own frontmatter: {screen}"
    );
    crate::pager::keys(
        &mut fixture.run.ui,
        &fixture.run.session.tree,
        key(KeyCode::Esc),
        still(scene().1),
    );
    fixture.paint(108, 30).await;
    fixture.press(typed('d')).await;
    assert!(
        fixture
            .paint(108, 30)
            .await
            .contains("the file and its index line go")
    );
    fixture.press(key(KeyCode::Enter)).await;
    assert!(!dir.join("rule.md").exists());
    assert_eq!(
        std::fs::read_to_string(dir.join("MEMORY.md")).unwrap(),
        "# Memory\nkeep\n"
    );
    assert!(!fixture.paint(108, 30).await.contains("Use fixtures"));
    fixture.run.ui.notice = None;
    fixture.press(typed('d')).await;
    let screen = fixture.paint(108, 30).await;
    assert!(screen.contains("edit the file itself"), "{screen}");
    assert!(fixture.env.config_dir.join("AGENTS.md").exists());
}
#[tokio::test]
async fn a_vanished_confirmation_never_forgets_the_next_row() {
    let mut fixture = MemoryFixture::new();
    let dir = fixture.seed();
    std::fs::write(dir.join("second.md"), "malformed").unwrap();
    fixture.open().await;
    fixture.paint(80, 24).await;
    fixture.press(typed('d')).await;
    fixture.paint(80, 24).await;
    std::fs::remove_file(dir.join("rule.md")).unwrap();
    fixture.press(key(KeyCode::Enter)).await;
    assert!(dir.join("second.md").exists());
}

#[tokio::test]
async fn escape_cancels_confirmation_then_closes_the_panel() {
    let mut fixture = MemoryFixture::new();
    let dir = fixture.seed();
    fixture.open().await;
    fixture.paint(80, 24).await;
    fixture.press(typed('d')).await;
    fixture.paint(80, 24).await;
    fixture.press(key(KeyCode::Esc)).await;
    assert!(
        !fixture
            .paint(80, 24)
            .await
            .contains("the file and its index line go")
    );
    assert!(dir.join("rule.md").exists());
    fixture.press(key(KeyCode::Esc)).await;
    assert!(crate::pane::placed(fixture.run.session.tree.viewed()).is_empty());
}

impl MemoryFixture {
    async fn forged(&self, target: serde_json::Value, confirming: serde_json::Value, key: &str) {
        let cx = self.cx();
        self.handler.run(
            &json!({"handler":"memory","event":"press","target":target,"confirming":confirming,"value":{"key":key}}).to_string(),
            &CommandContext { session: cx.session, cwd: cx.cwd, host: cx.host, cancel: Default::default() },
        ).await.unwrap();
    }
}
#[tokio::test]
async fn closed_and_mismatched_drawings_cannot_forget_a_listed_file() {
    for open in [false, true] {
        let mut fixture = MemoryFixture::new();
        let dir = fixture.seed();
        let second = dir.join("second.md");
        std::fs::write(&second, "malformed").unwrap();
        if open {
            fixture.open().await;
            fixture.paint(80, 24).await;
        }
        let target = json!(bingo_mod_memory::rows::Target::Memory {
            scope: bingo_services::memory::Scope::User,
            name: "second".into(),
            path: second.clone(),
        });
        fixture
            .forged(target.clone(), serde_json::Value::Null, "d")
            .await;
        fixture.forged(target.clone(), target, "enter").await;
        assert!(second.exists());
        assert!(dir.join("rule.md").exists());
    }
}
#[tokio::test]
async fn listings_stay_cached_until_forgetting_refreshes_them() {
    for (width, height) in [(80, 24), (108, 30)] {
        let mut fixture = MemoryFixture::new();
        let dir = fixture.seed();
        fixture.open().await;
        fixture.paint(width, height).await;
        std::fs::write(dir.join("second.md"), "malformed").unwrap();
        assert!(!fixture.paint(width, height).await.contains("second"));
        fixture.press(typed('d')).await;
        assert!(!fixture.paint(width, height).await.contains("second"));
        fixture.press(key(KeyCode::Enter)).await;
        let screen = fixture.paint(width, height).await;
        assert!(screen.contains("second"));
        assert!(screen.contains("Forgotten. Prompt refreshes later; say not to relearn it."));
    }
}
#[tokio::test]
async fn instruction_pager_uses_a_relative_title() {
    let mut fixture = MemoryFixture::new();
    fixture.seed();
    fixture.open().await;
    fixture.paint(80, 24).await;
    fixture.press(key(KeyCode::Down)).await;
    fixture.paint(80, 24).await;
    fixture.press(key(KeyCode::Enter)).await;
    let Open::Pager(pager) = fixture.run.ui.focus.open() else {
        panic!("pager");
    };
    let crate::pager::Source::Text { title, .. } = &pager.source else {
        panic!("text");
    };
    assert!(title.starts_with("./"));
    assert!(title.ends_with("AGENTS.md"));
}

#[tokio::test]
async fn memory_returns_to_focus_after_typing_and_reopening() {
    let mut fixture = MemoryFixture::new();
    fixture.open().await;
    fixture.paint(108, 30).await;
    fixture.press(typed('a')).await;
    assert_eq!(fixture.run.ui.composer.text(), "a");
    assert!(matches!(
        fixture.run.ui.focus,
        crate::focus::Focus::Composer
    ));
    fixture.paint(108, 30).await;
    fixture.run.terminal_event(Term::Key(key(KeyCode::Tab)));
    assert!(matches!(fixture.run.ui.focus, crate::focus::Focus::Pane(_)));
    fixture.press(key(KeyCode::Esc)).await;
    assert!(crate::pane::placed(fixture.run.session.tree.viewed()).is_empty());
    fixture.open().await;
    fixture.paint(108, 30).await;
    fixture.run.ui.focus = crate::focus::Focus::Composer;
    fixture.open().await;
    fixture.paint(108, 30).await;
    assert!(matches!(fixture.run.ui.focus, crate::focus::Focus::Pane(_)));
    assert_eq!(
        crate::pane::placed(fixture.run.session.tree.viewed()).len(),
        1
    );
    assert_eq!(fixture.run.ui.composer.text(), "a");
}

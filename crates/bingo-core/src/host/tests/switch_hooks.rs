use std::sync::Mutex;

use bingo_sdk::plugin::PluginSwitch;

use super::*;

struct Observed {
    id: &'static str,
    matcher: HookMatcher,
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Hook for Observed {
    fn id(&self) -> &str {
        self.id
    }

    fn matcher(&self) -> HookMatcher {
        self.matcher.clone()
    }

    async fn on_event(&self, _: &Frame, _: &HookContext) {
        self.seen.lock().unwrap().push(self.id.into());
    }

    async fn on_submit(&self, _: &mut Input, _: &HookContext) -> HookOutcome {
        HookOutcome::Deny {
            reason: self.id.into(),
        }
    }
}

fn observer(
    id: &'static str,
    points: Vec<HookPoint>,
    seen: &Arc<Mutex<Vec<String>>>,
) -> Arc<dyn Hook> {
    Arc::new(Observed {
        id,
        matcher: HookMatcher { points, tool: None },
        seen: seen.clone(),
    })
}

async fn build(home: &std::path::Path, initial: bool, hooks: Vec<Contribution>) -> Arc<Host> {
    let env = env_in(home);
    crate::plugins::switch(&env, HOOKS.id, initial).unwrap();
    let mut config = HostConfig::new(env.clone());
    config.layers = settings::load(&env, home, None).unwrap();
    Host::build(vec![TestPlugin::boxed(&HOOKS, hooks)], config)
        .await
        .unwrap()
}

fn context() -> HookContext {
    HookContext {
        item: None,
        session: SessionId::mint(),
        turn: None,
        cwd: Default::default(),
        provider: None,
        model: None,
        host: bingo_sdk::testing::NoHost::handle(),
    }
}

async fn event(hooks: &HookSet) {
    let cx = context();
    let frame = Frame {
        seq: Seq(1),
        ts: jiff::Timestamp::UNIX_EPOCH,
        session: cx.session.clone(),
        cause: None,
        event: Event::CatalogChanged {
            kind: "commands".into(),
        },
    };
    crate::chain::observe::work(
        crate::chain::observe::Work::Frame(Box::new(frame), cx),
        hooks,
    )
    .await;
}

#[tokio::test]
async fn event_only_hooks_switch_live_in_order_without_duplicate_delivery() {
    for initial in [true, false] {
        let home = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let hooks: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|id| observer(id, vec![HookPoint::Event], &seen))
            .collect();
        let host = build(
            home.path(),
            initial,
            hooks.iter().cloned().map(Contribution::Hook).collect(),
        )
        .await;
        assert_eq!(host.registry.switch_kind(HOOKS.id), Some(PluginSwitch::Now));
        // A session or running turn may already hold these observers.
        let held = host.hook_set().snapshot().await;
        for (held, original) in held.fixed.iter().zip(&hooks) {
            assert_eq!(held.id(), original.id());
            assert_eq!(held.matcher(), original.matcher());
        }
        event(&held).await;
        assert_eq!(seen.lock().unwrap().len(), if initial { 2 } else { 0 });
        for enabled in [false, true, true, false, true] {
            seen.lock().unwrap().clear();
            let switched = crate::plugins::switch_in(&host, &SessionId::mint(), HOOKS.id, enabled)
                .await
                .unwrap();
            assert!(matches!(switched,
                crate::plugins::Switched::Applied(now, PluginSwitch::Now) if now == enabled));
            event(&held).await;
            let expected: Vec<String> = if enabled {
                vec!["first".into(), "second".into()]
            } else {
                Vec::new()
            };
            assert_eq!(*seen.lock().unwrap(), expected, "enabled={enabled}");
        }
        host.shutdown().await;
    }
}

#[tokio::test]
async fn empty_mixed_and_other_hook_points_keep_their_restart_boundary() {
    let points = [
        HookPoint::Submit,
        HookPoint::BeforeTool,
        HookPoint::AfterTool,
        HookPoint::Stop,
        HookPoint::Turn,
        HookPoint::Compact,
        HookPoint::Session,
    ];
    let matchers = std::iter::once(Vec::new())
        .chain(points.into_iter().map(|point| vec![point]))
        .chain(
            points
                .into_iter()
                .map(|point| vec![HookPoint::Event, point]),
        );
    for points in matchers {
        let home = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let original = observer("held", points.clone(), &seen);
        let host = build(
            home.path(),
            true,
            vec![Contribution::Hook(original.clone())],
        )
        .await;
        assert_eq!(
            host.registry.switch_kind(HOOKS.id),
            Some(PluginSwitch::NextStart {
                why: "legacy hooks".into()
            }),
            "{points:?}",
        );
        let switched = crate::plugins::switch_in(&host, &SessionId::mint(), HOOKS.id, false)
            .await
            .unwrap();
        assert!(matches!(
            switched,
            crate::plugins::Switched::Applied(false, PluginSwitch::NextStart { .. })
        ));
        let held = host.hook_set().gather().await;
        assert_eq!(held.len(), 1);
        assert!(
            Arc::ptr_eq(&held[0], &original),
            "legacy hooks stay untouched"
        );
        if points.is_empty() || points.contains(&HookPoint::Submit) {
            let outcome = held[0]
                .on_submit(&mut Input::text("ask", Origin::surface("test")), &context())
                .await;
            assert_eq!(
                outcome,
                HookOutcome::Deny {
                    reason: "held".into()
                }
            );
        }
        host.shutdown().await;
    }
}

#[tokio::test]
async fn an_event_hook_from_a_source_still_needs_a_restart() {
    let home = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let source = ScriptedHookSource::new(vec![observer("late", vec![HookPoint::Event], &seen)]);
    let host = build(home.path(), true, vec![Contribution::Hooks(source)]).await;
    assert_eq!(
        host.registry.switch_kind(HOOKS.id),
        Some(PluginSwitch::NextStart {
            why: "legacy hooks".into()
        }),
    );
    let switched = crate::plugins::switch_in(&host, &SessionId::mint(), HOOKS.id, false)
        .await
        .unwrap();
    assert!(matches!(
        switched,
        crate::plugins::Switched::Applied(false, PluginSwitch::NextStart { .. })
    ));
    event(&host.hook_set()).await;
    assert_eq!(*seen.lock().unwrap(), ["late"]);
    host.shutdown().await;
}

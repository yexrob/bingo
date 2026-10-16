use std::sync::Mutex;

use bingo_sdk::plugin::PluginSwitch;

use super::*;

static OWNER: PluginManifest = PluginManifest {
    id: "fixture.owner",
    version: "0",
    uses: &[],
    provides: &[],
    requires: &[],
    config: None,
};
static OTHER: PluginManifest = PluginManifest {
    id: "fixture.other",
    ..OWNER
};

#[derive(Clone, Copy)]
enum Outcome {
    Done,
    Error,
    Panic,
    Wait,
}

struct Cleanup {
    label: &'static str,
    owner: &'static str,
    seen: Arc<Mutex<Vec<&'static str>>>,
    outcome: Outcome,
}

#[async_trait]
impl PluginLifecycle for Cleanup {
    async fn disabled(&self, host: &HostHandle) -> Result<(), KernelError> {
        let catalog = host.catalog(CatalogKind::Plugins).await?;
        let owner = catalog
            .entries
            .iter()
            .find(|entry| entry.id == self.owner)
            .unwrap();
        assert_eq!(owner.meta["state"]["kind"], "off");
        self.seen.lock().unwrap().push(self.label);
        match self.outcome {
            Outcome::Done => Ok(()),
            Outcome::Error => Err(KernelError::new(ErrorCode::Internal, "cleanup refused")),
            Outcome::Panic => panic!("cleanup panicked"),
            Outcome::Wait => std::future::pending().await,
        }
    }
}

fn cleanup(
    label: &'static str,
    owner: &'static str,
    seen: &Arc<Mutex<Vec<&'static str>>>,
    outcome: Outcome,
) -> Contribution {
    Contribution::Lifecycle(Arc::new(Cleanup {
        label,
        owner,
        seen: seen.clone(),
        outcome,
    }))
}

async fn build(config: HostConfig, contributions: Vec<Contribution>) -> Arc<Host> {
    Host::build(vec![TestPlugin::boxed(&OWNER, contributions)], config)
        .await
        .unwrap()
}

async fn switch(host: &Arc<Host>, enabled: bool) -> crate::plugins::Switched {
    crate::plugins::switch_in(host, &SessionId::mint(), OWNER.id, enabled)
        .await
        .unwrap()
}

#[tokio::test]
async fn lifecycle_runs_for_each_real_disable_and_only_its_owner() {
    for initial in [true, false] {
        let home = tempfile::tempdir().unwrap();
        let env = env_in(home.path());
        crate::plugins::switch(&env, OWNER.id, initial).unwrap();
        let mut config = HostConfig::new(env.clone());
        config.layers = settings::load(&env, home.path(), None).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let host = Host::build(
            vec![
                TestPlugin::boxed(
                    &OWNER,
                    vec![cleanup("owner", OWNER.id, &seen, Outcome::Done)],
                ),
                TestPlugin::boxed(
                    &OTHER,
                    vec![cleanup("other", OTHER.id, &seen, Outcome::Done)],
                ),
            ],
            config,
        )
        .await
        .unwrap();
        assert!(
            seen.lock().unwrap().is_empty(),
            "boot-off is not a transition"
        );
        let mut running = initial;
        let mut calls = 0;
        for enabled in [false, false, true, true, false] {
            assert!(matches!(switch(&host, enabled).await,
                crate::plugins::Switched::Applied(now, PluginSwitch::Now) if now == enabled));
            calls += usize::from(running && !enabled);
            running = enabled;
            assert_eq!(*seen.lock().unwrap(), vec!["owner"; calls]);
        }
        host.shutdown().await;
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["owner"; calls],
            "shutdown is separate"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn lifecycle_failures_do_not_undo_disable_or_skip_later_callbacks() {
    for failure in [Outcome::Error, Outcome::Panic, Outcome::Wait] {
        let home = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let host = build(
            HostConfig::new(env_in(home.path())),
            vec![
                cleanup("first", OWNER.id, &seen, failure),
                cleanup("second", OWNER.id, &seen, Outcome::Done),
            ],
        )
        .await;
        let mut client = log_client(&host).await;
        let (ack, frames) = client.ack("/modules disable fixture.owner").await;
        assert_eq!(commands::message(&ack), "fixture.owner is off now.");
        assert_eq!(*seen.lock().unwrap(), ["first", "second"]);
        assert!(frames.iter().any(|frame| matches!(&frame.event,
            Event::ItemCompleted { item } if matches!(&item.body,
                ItemBody::Notice { level: Level::Warn, code, .. }
                    if code == "PLUGIN_DISABLE_FAILED"))));
        let listed = crate::plugins::listing(host.registry()).await;
        assert!(matches!(
            listed
                .iter()
                .find(|entry| entry.id == OWNER.id)
                .unwrap()
                .state,
            PluginState::Off { .. }
        ));
        host.shutdown().await;
    }
}

#[tokio::test]
async fn next_start_and_overridden_wishes_never_deliver_lifecycle() {
    for restart in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut config = HostConfig::new(env_in(home.path()));
        let mut contributions = vec![cleanup("owner", OWNER.id, &seen, Outcome::Done)];
        if restart {
            contributions.push(Contribution::Tool(Arc::new(EchoTool { read_only: true })));
        } else {
            config = config.with_layer("cli", json!({"enabledPlugins":{"fixture.owner":true}}));
        }
        let host = build(config, contributions).await;
        let result = switch(&host, false).await;
        assert!(match result {
            crate::plugins::Switched::Applied(false, PluginSwitch::NextStart { .. }) => restart,
            crate::plugins::Switched::Overridden(true, _) => !restart,
            _ => false,
        });
        assert!(seen.lock().unwrap().is_empty());
        host.shutdown().await;
    }
}

#[tokio::test]
async fn reloaded_settings_override_controls_the_effective_disable() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let path = project.path().join("settings.json");
    std::fs::write(&path, r#"{"enabledPlugins":{"fixture.owner":true}}"#).unwrap();
    let env = env_in(home.path());
    let mut config = HostConfig::new(env.clone());
    config.layers = settings::load(&env, project.path(), Some(&path)).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let host = build(
        config,
        vec![cleanup("owner", OWNER.id, &seen, Outcome::Done)],
    )
    .await;
    assert!(matches!(
        switch(&host, false).await,
        crate::plugins::Switched::Overridden(true, _)
    ));
    std::fs::write(&path, "{broken").unwrap();
    assert!(
        crate::plugins::switch_in(&host, &SessionId::mint(), OWNER.id, false)
            .await
            .is_err()
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "rejected settings never disable"
    );
    std::fs::write(&path, "{}").unwrap();
    assert!(matches!(
        switch(&host, false).await,
        crate::plugins::Switched::Applied(false, PluginSwitch::Now)
    ));
    assert_eq!(*seen.lock().unwrap(), ["owner"]);
    host.shutdown().await;
}

async fn log_client(host: &Host) -> commands::Client {
    let attachment = host
        .open(
            SessionSelector::Create {
                spec: SessionSpec {
                    driver: Driver::Log,
                    ..spec("/work")
                },
            },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    commands::Client {
        state: attachment.snapshot,
        events: attachment.events,
        handle: attachment.handle,
    }
}

struct Signalling(Arc<Mutex<Option<SessionId>>>);

#[async_trait]
impl PluginLifecycle for Signalling {
    async fn disabled(&self, host: &HostHandle) -> Result<(), KernelError> {
        let session = self.0.lock().unwrap().clone().unwrap();
        host.signal(&session, OWNER.id, "pane", Value::Null).await
    }
}

async fn action(client: &mut commands::Client, name: &str, args: &str) -> IntentOutcome {
    let intent = IntentId::mint();
    client.handle.submit(
        intent.clone(),
        Input::action(
            Action {
                name: name.into(),
                args: json!(args),
            },
            Origin::surface("rpc"),
        ),
    );
    while let Some(frame) = client.events.next().await {
        client.state.apply(&frame);
        if let Event::IntentAck {
            intent: answered,
            outcome,
        } = frame.event
            && answered == intent
        {
            return outcome;
        }
    }
    panic!("the action is acknowledged");
}

#[tokio::test]
async fn command_alias_and_rpc_action_share_the_owner_lifecycle() {
    let home = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let session = Arc::new(Mutex::new(None));
    let host = build(
        HostConfig::new(env_in(home.path())),
        vec![
            cleanup("owner", OWNER.id, &seen, Outcome::Done),
            Contribution::Lifecycle(Arc::new(Signalling(session.clone()))),
        ],
    )
    .await;
    let mut client = log_client(&host).await;
    *session.lock().unwrap() = Some(client.state.summary.id.clone());
    host.signal(
        &client.state.summary.id,
        OWNER.id,
        "pane",
        json!({"open":true}),
    )
    .await
    .unwrap();
    let (ack, _) = client.ack("/modules disable fixture.owner").await;
    assert_eq!(commands::message(&ack), "fixture.owner is off now.");
    assert!(!client.state.signals.contains_key(OWNER.id));
    assert_eq!(*seen.lock().unwrap(), ["owner"]);
    action(&mut client, "plugins", "enable fixture.owner").await;
    host.signal(
        &client.state.summary.id,
        OWNER.id,
        "pane",
        json!({"open":true}),
    )
    .await
    .unwrap();
    let ack = action(&mut client, "plugins", "disable fixture.owner").await;
    assert_eq!(commands::message(&ack), "fixture.owner is off now.");
    assert!(!client.state.signals.contains_key(OWNER.id));
    assert_eq!(*seen.lock().unwrap(), ["owner", "owner"]);
    host.shutdown().await;
}

struct Deferred;

#[async_trait]
impl PluginSource for Deferred {
    fn id(&self) -> &str {
        "fixture.source"
    }
    async fn plugins(&self) -> Vec<PluginStatus> {
        vec![PluginStatus {
            id: "plugins:external".into(),
            version: "0".into(),
            state: PluginState::On,
            from: self.id().into(),
            placement: None,
            grant: None,
        }]
    }
    async fn switch_kind(&self, _: &str) -> PluginSwitch {
        PluginSwitch::Now
    }
    async fn switch(&self, _: &str, _: bool) -> Result<PluginSwitch, KernelError> {
        Ok(PluginSwitch::AfterTurn)
    }
}

#[tokio::test]
async fn prefixed_source_switches_do_not_disable_the_native_owner() {
    let home = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let host = build(
        HostConfig::new(env_in(home.path())),
        vec![
            cleanup("owner", OWNER.id, &seen, Outcome::Done),
            Contribution::Plugins(Arc::new(Deferred)),
        ],
    )
    .await;
    let mut client = log_client(&host).await;
    let (ack, _) = client.ack("/modules disable plugins:external").await;
    assert_eq!(
        commands::message(&ack),
        "external is off after the running turn; switch queued."
    );
    assert!(seen.lock().unwrap().is_empty());
    host.shutdown().await;
}

struct Dropped(Arc<std::sync::atomic::AtomicBool>);

impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[derive(Default)]
struct HeldCleanup {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    finished: std::sync::atomic::AtomicBool,
    dropped: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl PluginLifecycle for HeldCleanup {
    async fn disabled(&self, _: &HostHandle) -> Result<(), KernelError> {
        let _drop = Dropped(self.dropped.clone());
        self.entered.notify_one();
        self.release.notified().await;
        self.finished
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn enable_waits_for_cleanup_and_timeout_drops_late_work() {
    use std::sync::atomic::Ordering;

    for timeout in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let cleanup = Arc::new(HeldCleanup::default());
        let host = build(
            HostConfig::new(env_in(home.path())),
            vec![Contribution::Lifecycle(cleanup.clone())],
        )
        .await;
        let disabling = tokio::spawn({
            let host = host.clone();
            async move { switch(&host, false).await }
        });
        cleanup.entered.notified().await;
        assert!(host.registry.switches.switching.try_lock().is_err());
        let (starting, started) = tokio::sync::oneshot::channel();
        let enabling = tokio::spawn({
            let host = host.clone();
            async move {
                starting.send(()).unwrap();
                switch(&host, true).await
            }
        });
        started.await.unwrap();
        assert!(
            !enabling.is_finished(),
            "enable cannot overtake old cleanup"
        );
        if timeout {
            tokio::time::advance(HOOK_DEADLINE).await;
        } else {
            cleanup.release.notify_one();
        }
        assert!(matches!(
            disabling.await.unwrap(),
            crate::plugins::Switched::Applied(false, PluginSwitch::Now)
        ));
        assert!(matches!(
            enabling.await.unwrap(),
            crate::plugins::Switched::Applied(true, PluginSwitch::Now)
        ));
        assert!(cleanup.dropped.load(Ordering::SeqCst));
        cleanup.release.notify_one();
        tokio::task::yield_now().await;
        assert_eq!(cleanup.finished.load(Ordering::SeqCst), !timeout);
        assert!(
            !host
                .registry
                .switches
                .running
                .read()
                .unwrap()
                .contains(OWNER.id)
        );
        host.shutdown().await;
    }
}

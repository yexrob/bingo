//! A remembered pair is a startup fallback, never a reconfiguration hook.

use super::commands::{Client, message};
use super::*;
use crate::models::selection::{self, Selection};
use crate::settings::{Layer, LayerOrigin};
use std::path::Path;

fn record(home: &Path) -> std::path::PathBuf {
    env_in(home).data_dir.join("last-model.json")
}

fn plant(home: &Path, provider: &str, model: &str) {
    selection::save(
        &record(home),
        &Selection::new(provider.into(), model.into()),
    )
    .unwrap();
}

fn config(home: &Path) -> HostConfig {
    let mut config = HostConfig::new(env_in(home));
    config.layers.push(Layer::new(
        LayerOrigin::User,
        "user",
        json!({
            "provider":"scripted", "model":"user-model", "thinking":"low", "maxTokens":1234
        })
        .as_object()
        .unwrap()
        .clone(),
    ));
    config
}

async fn build(
    config: HostConfig,
    missing: bool,
    store: Option<Arc<dyn SessionStore>>,
) -> Arc<Host> {
    let first = ScriptedProvider::new(vec![]);
    first.serves(&["fallback"]);
    let second = ScriptedProvider::named("other", vec![]);
    let second = if missing {
        second.missing("sign in")
    } else {
        second
    };
    let mut plugins = vec![TestPlugin::boxed(
        &PROVIDER,
        vec![
            Contribution::Provider(first),
            Contribution::Provider(second),
        ],
    )];
    if let Some(store) = store {
        plugins.push(TestPlugin::boxed(&STORE, vec![Contribution::Store(store)]));
    }
    Host::build(plugins, config).await.unwrap()
}

#[tokio::test]
async fn a_successful_root_choice_restores_the_pair_without_changing_settings_or_effort() {
    let home = tempfile::tempdir().unwrap();
    let path = settings::user_path(&env_in(home.path()));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text =
        "# handwritten\nprovider = \"scripted\"\nmodel = \"user-model\"\nthinking = \"low\"\n";
    std::fs::write(&path, text).unwrap();
    let host = build(config(home.path()), false, None).await;
    let mut client = Client::open(&host).await;
    assert!(!record(home.path()).exists(), "startup never writes");
    let (ack, _) = client.ack("/model other/org/unlisted").await;
    assert!(message(&ack).contains("next-start default"));
    client.ack("/think high").await;
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    let saved = std::fs::read(record(home.path())).unwrap();
    let same_process = Client::open(&host).await;
    assert_eq!(
        same_process.state.summary.model.as_deref(),
        Some("user-model")
    );
    host.shutdown().await;

    let next = build(config(home.path()), false, None).await;
    let opened = Client::open(&next).await;
    assert_eq!(opened.state.summary.provider.as_deref(), Some("other"));
    assert_eq!(opened.state.summary.model.as_deref(), Some("org/unlisted"));
    assert_eq!(opened.state.config.kernel["thinking"], "low");
    assert_eq!(next.settings.kernel.max_tokens, Some(1234));
    assert_eq!(std::fs::read(record(home.path())).unwrap(), saved);
    next.shutdown().await;
}

#[tokio::test]
async fn every_higher_layer_suppresses_the_pair_even_when_only_one_key_is_present() {
    let home = tempfile::tempdir().unwrap();
    plant(home.path(), "other", "remembered");
    for origin in [
        LayerOrigin::Explicit,
        LayerOrigin::Repository(home.path().into()),
    ] {
        for (value, provider, model) in [
            (json!({"provider":"scripted"}), "scripted", "user-model"),
            (json!({"model":"override"}), "scripted", "override"),
            (json!({"provider":null}), "scripted", "user-model"),
            (json!({"model":null}), "scripted", "fallback"),
        ] {
            let mut config = config(home.path());
            config.layers.push(Layer::new(
                origin.clone(),
                "higher",
                value.as_object().unwrap().clone(),
            ));
            let host = build(config, false, None).await;
            let opened = Client::open(&host).await;
            assert_eq!(opened.state.summary.provider.as_deref(), Some(provider));
            assert_eq!(opened.state.summary.model.as_deref(), Some(model));
            assert_eq!(opened.state.config.kernel["thinking"], "low");
            host.shutdown().await;
        }
    }
}

#[tokio::test]
async fn unrelated_higher_settings_do_not_suppress_the_remembered_pair() {
    let home = tempfile::tempdir().unwrap();
    plant(home.path(), "other", "remembered");
    let config =
        config(home.path()).with_layer("cli", json!({"thinking":"high", "maxTokens":5678}));
    let host = build(config, false, None).await;
    let opened = Client::open(&host).await;
    assert_eq!(opened.state.summary.provider.as_deref(), Some("other"));
    assert_eq!(opened.state.summary.model.as_deref(), Some("remembered"));
    assert_eq!(opened.state.config.kernel["thinking"], "high");
    assert_eq!(host.settings.kernel.max_tokens, Some(5678));
    host.shutdown().await;
}

#[tokio::test]
async fn stale_missing_unsigned_in_and_corrupt_records_fall_back_without_rewriting() {
    let home = tempfile::tempdir().unwrap();
    for (provider, missing) in [("removed", false), ("other", true)] {
        plant(home.path(), provider, "stale");
        let before = std::fs::read(record(home.path())).unwrap();
        let host = build(config(home.path()), missing, None).await;
        assert_eq!(
            Client::open(&host).await.state.summary.model.as_deref(),
            Some("user-model")
        );
        assert_eq!(std::fs::read(record(home.path())).unwrap(), before);
        host.shutdown().await;
    }
    std::fs::write(record(home.path()), b"broken").unwrap();
    let host = build(config(home.path()), false, None).await;
    assert_eq!(
        Client::open(&host).await.state.summary.model.as_deref(),
        Some("user-model")
    );
    assert_eq!(std::fs::read(record(home.path())).unwrap(), b"broken");
    host.shutdown().await;
}

#[tokio::test]
async fn children_reconfigure_reports_and_failed_commands_do_not_remember() {
    let home = tempfile::tempdir().unwrap();
    plant(home.path(), "other", "remembered");
    let before = std::fs::read(record(home.path())).unwrap();
    let host = build(config(home.path()), false, None).await;
    let mut root = Client::open(&host).await;
    root.ack("/model").await;
    let (ack, _) = root.ack("/model /").await;
    assert!(matches!(ack, IntentOutcome::Rejected { .. }));
    host.reconfigure(
        &root.state.summary.id,
        SessionChange::Model {
            provider: Some("scripted".into()),
            model: "api-choice".into(),
            scope: ChoiceScope::Session,
        },
    )
    .await
    .unwrap();
    let child = host
        .open(
            SessionSelector::Create {
                spec: SessionSpec {
                    parent: Some(ParentLink {
                        session: root.state.summary.id.clone(),
                        item: None,
                    }),
                    ..spec("/work")
                },
            },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    let mut child = Client {
        state: child.snapshot,
        events: child.events,
        handle: child.handle,
    };
    let (ack, _) = child.ack("/model child-choice").await;
    assert_eq!(
        message(&ack),
        "⇄ child-choice · scripted · thinking low · this session"
    );
    assert_eq!(std::fs::read(record(home.path())).unwrap(), before);
    host.shutdown().await;
}

#[tokio::test]
async fn resume_keeps_its_journal_choice_and_does_not_remember_it() {
    let home = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::journal::MemoryStore::new());
    let host = build(config(home.path()), false, Some(store.clone())).await;
    let root = Client::open(&host).await;
    let id = root.state.summary.id.clone();
    host.reconfigure(
        &id,
        SessionChange::Model {
            provider: Some("scripted".into()),
            model: "journal-choice".into(),
            scope: ChoiceScope::Session,
        },
    )
    .await
    .unwrap();
    host.shutdown().await;
    plant(home.path(), "other", "remembered");
    let before = std::fs::read(record(home.path())).unwrap();
    let next = build(config(home.path()), false, Some(store)).await;
    let reopened = next
        .open(SessionSelector::ById { id }, who(), OpenOptions::default())
        .await
        .unwrap();
    assert_eq!(
        reopened.snapshot.summary.provider.as_deref(),
        Some("scripted")
    );
    assert_eq!(
        reopened.snapshot.summary.model.as_deref(),
        Some("journal-choice")
    );
    assert_eq!(std::fs::read(record(home.path())).unwrap(), before);
    next.shutdown().await;
}

struct RewriteModel;

#[async_trait]
impl Interceptor for RewriteModel {
    fn id(&self) -> &str {
        "rewrite-model"
    }
    fn matcher(&self) -> LinkMatcher {
        LinkMatcher::at(bingo_sdk::catalogue::CONFIG_SET)
    }
    async fn chain(
        &self,
        _: &bingo_sdk::catalogue::Entry,
        _: &HookContext,
        mut value: Value,
        next: Next<'_, Value, Value>,
    ) -> Result<Value, KernelError> {
        if value["key"] == "model" && value["value"] == "requested" {
            value["value"] = json!("applied");
        }
        next.run(value).await
    }
}

#[tokio::test]
async fn the_remembered_pair_is_the_applied_choice_after_config_interceptors() {
    let home = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![]);
    let plugins = vec![TestPlugin::boxed(
        &PROVIDER,
        vec![
            Contribution::Provider(provider),
            Contribution::Interceptor {
                placement: Default::default(),
                external: None,
                link: Arc::new(RewriteModel),
            },
        ],
    )];
    let host = Host::build(plugins, config(home.path())).await.unwrap();
    let mut client = Client::open(&host).await;
    client.ack("/model requested").await;
    let saved = selection::load(&record(home.path())).unwrap();
    assert_eq!(saved.provider, "scripted");
    assert_eq!(saved.model, "applied");
    assert_eq!(
        host.session_model(&client.state.summary.id)
            .await
            .unwrap()
            .unwrap()
            .id,
        "applied"
    );
    host.shutdown().await;
}

struct OpensAtStart(Arc<Mutex<Option<SessionSummary>>>);

#[async_trait]
impl Plugin for OpensAtStart {
    fn manifest(&self) -> &'static PluginManifest {
        &STORE
    }
    fn register(&self, _: &mut Registrar) -> Result<(), PluginError> {
        Ok(())
    }
    async fn start(&self, host: HostHandle) -> Result<(), PluginError> {
        let opened = host
            .open(
                SessionSelector::Create {
                    spec: spec("/work"),
                },
                who(),
                OpenOptions::default(),
            )
            .await
            .map_err(|error| PluginError::Failed(error.to_string()))?;
        *self.0.lock().unwrap() = Some(opened.snapshot.summary);
        Ok(())
    }
}

#[tokio::test]
async fn plugins_start_on_the_same_remembered_snapshot_as_later_sessions() {
    let home = tempfile::tempdir().unwrap();
    plant(home.path(), "scripted", "remembered");
    let opened = Arc::new(Mutex::new(None));
    let plugins = vec![
        TestPlugin::boxed(
            &PROVIDER,
            vec![Contribution::Provider(ScriptedProvider::new(vec![]))],
        ),
        Box::new(OpensAtStart(opened.clone())) as Box<dyn Plugin>,
    ];
    let host = Host::build(plugins, config(home.path())).await.unwrap();
    let summary = opened.lock().unwrap().clone().unwrap();
    assert_eq!(summary.model.as_deref(), Some("remembered"));
    assert_eq!(
        host.session_model(&summary.id).await.unwrap().unwrap().id,
        "remembered"
    );
    assert_eq!(
        Client::open(&host).await.state.summary.model.as_deref(),
        Some("remembered")
    );
    host.shutdown().await;
}

struct ChoosesAtStart;

#[async_trait]
impl Plugin for ChoosesAtStart {
    fn manifest(&self) -> &'static PluginManifest {
        &STORE
    }
    fn register(&self, _: &mut Registrar) -> Result<(), PluginError> {
        Ok(())
    }
    async fn start(&self, host: HostHandle) -> Result<(), PluginError> {
        let opened = host
            .open(
                SessionSelector::Create {
                    spec: spec("/work"),
                },
                who(),
                OpenOptions::default(),
            )
            .await
            .map_err(|error| PluginError::Failed(error.to_string()))?;
        let mut client = Client {
            state: opened.snapshot,
            events: opened.events,
            handle: opened.handle,
        };
        let (ack, _) = client.ack("/model startup-choice").await;
        assert!(message(&ack).contains("next-start default"));
        Ok(())
    }
}

#[tokio::test]
async fn a_choice_during_plugin_start_is_for_the_next_process_not_later_roots() {
    let home = tempfile::tempdir().unwrap();
    let plugins = vec![
        TestPlugin::boxed(
            &PROVIDER,
            vec![Contribution::Provider(ScriptedProvider::new(vec![]))],
        ),
        Box::new(ChoosesAtStart) as Box<dyn Plugin>,
    ];
    let host = Host::build(plugins, config(home.path())).await.unwrap();
    assert_eq!(
        selection::load(&record(home.path())).unwrap().model,
        "startup-choice"
    );
    assert_eq!(
        Client::open(&host).await.state.summary.model.as_deref(),
        Some("user-model")
    );
    host.shutdown().await;
}

struct PublishesAtStart(Arc<ScriptedProviderSource>);

#[async_trait]
impl Plugin for PublishesAtStart {
    fn manifest(&self) -> &'static PluginManifest {
        &TOOLS
    }
    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        registrar.add(Contribution::Providers(self.0.clone()));
        Ok(())
    }
    async fn start(&self, _: HostHandle) -> Result<(), PluginError> {
        self.0.set(vec![ScriptedProvider::named("late", vec![])]);
        Ok(())
    }
}

#[tokio::test]
async fn a_provider_published_at_start_is_restored_without_changing_an_early_roots_choice() {
    let home = tempfile::tempdir().unwrap();
    plant(home.path(), "late", "remembered");
    let early = Arc::new(Mutex::new(None));
    let plugins = vec![
        TestPlugin::boxed(
            &PROVIDER,
            vec![Contribution::Provider(ScriptedProvider::new(vec![]))],
        ),
        Box::new(OpensAtStart(early.clone())) as Box<dyn Plugin>,
        Box::new(PublishesAtStart(ScriptedProviderSource::new(vec![]))) as Box<dyn Plugin>,
    ];
    let host = Host::build(plugins, config(home.path())).await.unwrap();
    let summary = early.lock().unwrap().clone().unwrap();
    let reported = host.session_model(&summary.id).await.unwrap().unwrap();
    assert_eq!(summary.provider.as_deref(), Some("scripted"));
    assert_eq!(summary.model.as_deref(), Some("user-model"));
    assert_eq!(reported.endpoint.id(), "scripted");
    assert_eq!(reported.id, "user-model");
    let opened = Client::open(&host).await;
    assert_eq!(opened.state.summary.provider.as_deref(), Some("late"));
    assert_eq!(opened.state.summary.model.as_deref(), Some("remembered"));
    host.shutdown().await;
}

struct PausedSource {
    armed: std::sync::atomic::AtomicBool,
    release: tokio::sync::Notify,
}

#[async_trait]
impl ProviderSource for PausedSource {
    fn id(&self) -> &str {
        "paused"
    }
    async fn providers(&self) -> Vec<Arc<dyn Provider>> {
        if self.armed.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.release.notified().await;
        }
        vec![ScriptedProvider::named("late", vec![])]
    }
}

#[tokio::test]
async fn an_in_flight_open_snapshots_both_defaults_including_an_absent_model() {
    for declared_model in [Some("user-model"), None] {
        let home = tempfile::tempdir().unwrap();
        let mut config = config(home.path());
        if declared_model.is_none() {
            config.layers[0].value.remove("model");
        }
        let source = Arc::new(PausedSource {
            armed: false.into(),
            release: tokio::sync::Notify::new(),
        });
        let provider = ScriptedProvider::new(vec![]);
        provider.serves(&["fallback"]);
        let plugins = vec![TestPlugin::boxed(
            &PROVIDER,
            vec![
                Contribution::Provider(provider),
                Contribution::Providers(source.clone()),
            ],
        )];
        let host = Host::build(plugins, config).await.unwrap();
        plant(home.path(), "late", "remembered");
        let candidate = super::super::remembered::load_model(&host);
        source
            .armed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let mut opening = Box::pin(host.open(
            SessionSelector::Create {
                spec: spec("/work"),
            },
            who(),
            OpenOptions::default(),
        ));
        assert!(futures::poll!(&mut opening).is_pending());
        super::super::remembered::restore_model(&host, candidate.as_ref()).await;
        assert!(host.remembered.get().is_some());
        source.release.notify_one();
        let opened = opening.await.unwrap();
        assert_eq!(
            opened.snapshot.summary.provider.as_deref(),
            Some("scripted")
        );
        assert_eq!(
            opened.snapshot.summary.model.as_deref(),
            Some(declared_model.unwrap_or("fallback"))
        );
        let reported = host.session_model(&opened.session).await.unwrap().unwrap();
        assert_eq!(reported.endpoint.id(), "scripted");
        assert_eq!(reported.id, declared_model.unwrap_or("fallback"));
        assert_eq!(
            opened.snapshot.summary.model_scope,
            Some(ChoiceScope::Default)
        );
        host.reconfigure(&opened.session, SessionChange::Thinking(Some(Effort::High)))
            .await
            .unwrap();
        assert_eq!(
            host.session_state(&opened.session)
                .await
                .unwrap()
                .summary
                .model_scope,
            Some(ChoiceScope::Default)
        );
        host.shutdown().await;
    }
}

#[cfg(windows)]
#[tokio::test]
async fn keep_warns_when_an_old_readable_preference_cannot_be_replaced() {
    use std::os::windows::fs::OpenOptionsExt;
    let home = tempfile::tempdir().unwrap();
    plant(home.path(), "other", "old-choice");
    let host = build(config(home.path()), false, None).await;
    let mut client = Client::open(&host).await;
    let locked = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(record(home.path()))
        .unwrap();
    let (ack, _) = client.ack("/model scripted/new-choice --keep").await;
    assert!(
        message(&ack).starts_with("⇄ new-choice · scripted · thinking low · kept for new sessions")
    );
    assert!(message(&ack).contains("could not remember next-start default"));
    assert_eq!(client.state.summary.model.as_deref(), Some("new-choice"));
    drop(locked);
    assert_eq!(
        selection::load(&record(home.path())).unwrap().model,
        "old-choice"
    );
    let settings = settings::read_document(&settings::user_path(host.env())).unwrap();
    assert_eq!(settings["provider"], "scripted");
    assert_eq!(settings["model"], "new-choice");
    host.shutdown().await;
}

#[tokio::test]
async fn preference_write_failure_does_not_undo_the_successful_switch() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(record(home.path())).unwrap();
    let host = build(config(home.path()), false, None).await;
    let mut client = Client::open(&host).await;
    let (ack, _) = client.ack("/model other/new-choice").await;
    assert!(message(&ack).contains("could not remember next-start default"));
    assert_eq!(client.state.summary.model.as_deref(), Some("new-choice"));
    host.shutdown().await;
}

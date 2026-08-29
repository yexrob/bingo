use std::sync::Arc;

use bingo_sdk::*;
use futures::StreamExt;
use serde_json::json;

use super::*;
use crate::test_support::*;

mod commands;
mod policy;

/// A plugin assembled from parts, so tests can shape manifests freely.
struct TestPlugin {
    manifest: &'static PluginManifest,
    contributions: std::sync::Mutex<Vec<Contribution>>,
}

impl TestPlugin {
    fn boxed(
        manifest: &'static PluginManifest,
        contributions: Vec<Contribution>,
    ) -> Box<dyn Plugin> {
        Box::new(Self {
            manifest,
            contributions: std::sync::Mutex::new(contributions),
        })
    }
}

#[async_trait]
impl Plugin for TestPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        self.manifest
    }
    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        for c in self.contributions.lock().unwrap().drain(..) {
            registrar.add(c);
        }
        Ok(())
    }
}

static PROVIDER: PluginManifest = PluginManifest {
    id: "test.provider",
    version: "0",
    sdk: "^0.1",
    provides: &["provider:scripted"],
    requires: &[],
    config: None,
};

static TOOLS: PluginManifest = PluginManifest {
    id: "test.tools",
    version: "0",
    sdk: "^0.1",
    provides: &["tool:Echo"],
    requires: &["provider:scripted"],
    config: None,
};

static NEEDY: PluginManifest = PluginManifest {
    id: "test.needy",
    version: "0",
    sdk: "^0.1",
    provides: &[],
    requires: &["service:missing"],
    config: None,
};

fn env() -> Env {
    Env {
        home: "/tmp".into(),
        config_dir: "/tmp".into(),
        data_dir: "/tmp".into(),
    }
}

fn who() -> ClientIdentity {
    ClientIdentity {
        name: "test".into(),
        surface: "test".into(),
    }
}

fn spec(cwd: &str) -> SessionSpec {
    SessionSpec {
        cwd: cwd.into(),
        ..SessionSpec::default()
    }
}

async fn host_with(scripts: Vec<Script>) -> (Arc<Host>, Arc<ScriptedProvider>) {
    let provider = ScriptedProvider::new(scripts);
    let plugins = vec![
        TestPlugin::boxed(&PROVIDER, vec![Contribution::Provider(provider.clone())]),
        TestPlugin::boxed(
            &TOOLS,
            vec![Contribution::Tool(Arc::new(EchoTool { read_only: true }))],
        ),
        TestPlugin::boxed(&NEEDY, vec![]),
    ];
    let config = HostConfig::new(env())
        .with_layer("user", json!({"model": "u", "theme": "dark"}))
        .with_layer("cli", json!({"model": "m"}));
    (Host::build(plugins, config).await.unwrap(), provider)
}

#[tokio::test]
async fn plugins_load_in_order_and_unmet_requirements_disable_not_crash() {
    let (host, _) = host_with(vec![]).await;
    let statuses = &host.registry().plugins;
    assert_eq!(
        statuses
            .iter()
            .map(|p| (p.id.as_str(), p.enabled))
            .collect::<Vec<_>>(),
        vec![
            ("test.provider", true),
            ("test.tools", true),
            ("test.needy", false)
        ]
    );
    assert_eq!(
        statuses[2].reason.as_deref(),
        Some("unmet requirements: service:missing")
    );
    let plugins = host.catalog(CatalogKind::Plugins).await.unwrap();
    assert_eq!(plugins.entries[2].meta["enabled"], json!(false));
    assert_eq!(
        host.catalog(CatalogKind::Tools).await.unwrap().entries[0].id,
        "Echo"
    );
    assert_eq!(
        host.catalog(CatalogKind::Providers).await.unwrap().entries[0].id,
        "scripted"
    );
    assert_eq!(
        host.catalog(CatalogKind::Models).await.unwrap().entries[0].id,
        "scripted/m"
    );
}

#[tokio::test]
async fn a_second_policy_is_a_conflict() {
    struct P;
    #[async_trait]
    impl PermissionPolicy for P {
        fn id(&self) -> &str {
            "p"
        }
        async fn decide(&self, _: PolicyInput<'_>) -> Decision {
            Decision::Deny {
                reason: Reason::Default,
            }
        }
    }
    let plugins = vec![TestPlugin::boxed(
        &PROVIDER,
        vec![
            Contribution::Policy(Arc::new(P)),
            Contribution::Policy(Arc::new(P)),
        ],
    )];
    let err = Host::build(plugins, HostConfig::new(env()))
        .await
        .err()
        .unwrap();
    assert!(
        matches!(err, HostError::Conflict { ref plugin, .. } if plugin == "test.provider"),
        "{err}"
    );
}

#[tokio::test]
async fn open_create_runs_a_turn_and_the_session_is_findable_afterwards() {
    let (host, provider) = host_with(vec![Script::Events(text("hello"))]).await;
    let mut gateway = host.gateway_events();
    let Attachment {
        session,
        mut snapshot,
        mut events,
        handle,
    } = host
        .open(
            SessionSelector::Create {
                spec: SessionSpec {
                    key: Some("host/one".into()),
                    ..spec("/work")
                },
            },
            who(),
        )
        .await
        .unwrap();
    assert!(
        matches!(gateway.next().await, Some(GatewayEvent::SessionCreated { summary }) if summary.id == session)
    );
    assert_eq!(snapshot.summary.provider.as_deref(), Some("scripted"));
    assert_eq!(snapshot.summary.model.as_deref(), Some("m"));

    handle.submit(IntentId::mint(), Input::text("hi", Origin::surface("test")));
    while let Some(frame) = events.next().await {
        snapshot.apply(&frame);
        if matches!(frame.event, Event::TurnCompleted { .. }) {
            break;
        }
    }
    assert_eq!(snapshot.last_turn, Some(TurnStatus::Completed));
    let request = &provider.requests()[0];
    assert!(request.system[0].text.starts_with("You are bingo"));
    assert!(
        request.system[0].cache,
        "the identity block is the cache prefix"
    );
    assert!(request.system[1].text.contains("Working directory: /work"));
    assert_eq!(
        host.notices(),
        vec![(
            "UNKNOWN_SETTING".to_string(),
            "unknown setting `theme` in user".to_string()
        )]
    );
    assert_eq!(
        request
            .tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Echo"]
    );

    let listed = host.sessions(SessionFilter::default()).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].key.as_deref(), Some("host/one"));
    assert!(!listed[0].busy);
    for selector in [
        SessionSelector::ById {
            id: session.clone(),
        },
        SessionSelector::ByKey {
            key: "host/one".into(),
        },
        SessionSelector::Latest {
            cwd: "/work".into(),
        },
    ] {
        let again = host.open(selector, who()).await.unwrap();
        assert_eq!(again.session, session);
        assert_eq!(again.snapshot.items.len(), 2, "reopening sees the history");
    }
    assert_eq!(
        host.open(
            SessionSelector::Latest {
                cwd: "/elsewhere".into()
            },
            who()
        )
        .await
        .err()
        .map(|e| e.code),
        Some(ErrorCode::SessionNotFound)
    );
    assert_eq!(
        host.open(
            SessionSelector::Create {
                spec: SessionSpec {
                    key: Some("host/one".into()),
                    ..spec("/work")
                }
            },
            who()
        )
        .await
        .err()
        .map(|e| e.code),
        Some(ErrorCode::SessionLocked)
    );

    host.delete(&session).await.unwrap();
    assert!(
        matches!(gateway.next().await, Some(GatewayEvent::SessionRemoved { session: s }) if s == session)
    );
    assert!(
        host.sessions(SessionFilter::default())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn sub_sessions_are_sessions_with_a_parent_and_a_depth_limit() {
    let (host, _) = host_with(vec![]).await;
    let root = host
        .open(
            SessionSelector::Create {
                spec: spec("/work"),
            },
            who(),
        )
        .await
        .unwrap();
    let link = ParentLink {
        session: root.session.clone(),
        item: ItemId::mint(),
    };
    let child = host
        .open(
            SessionSelector::Create {
                spec: SessionSpec {
                    parent: Some(link.clone()),
                    tools: Some(vec![]),
                    ..spec("/work")
                },
            },
            who(),
        )
        .await
        .unwrap();
    assert_eq!(child.snapshot.summary.parent, Some(link));
    let children = host
        .sessions(SessionFilter {
            parent: Some(root.session.clone()),
            ..SessionFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, child.session);

    let grandchild = host
        .open(
            SessionSelector::Create {
                spec: SessionSpec {
                    parent: Some(ParentLink {
                        session: child.session.clone(),
                        item: ItemId::mint(),
                    }),
                    ..spec("/work")
                },
            },
            who(),
        )
        .await;
    assert_eq!(
        grandchild.err().map(|e| e.code),
        Some(ErrorCode::InvalidInput)
    );
}

#[tokio::test]
async fn opening_without_a_provider_or_model_says_so() {
    let host = Host::build(vec![], HostConfig::new(env())).await.unwrap();
    assert_eq!(
        host.open(
            SessionSelector::Create {
                spec: spec("/work")
            },
            who()
        )
        .await
        .err()
        .map(|e| e.code),
        Some(ErrorCode::ProviderUnavailable)
    );
    let (host, _) = host_with(vec![]).await;
    assert_eq!(
        host.open(
            SessionSelector::Create {
                spec: SessionSpec {
                    provider: Some("nope".into()),
                    ..spec("/work")
                }
            },
            who()
        )
        .await
        .err()
        .map(|e| e.code),
        Some(ErrorCode::ProviderUnavailable)
    );
}

#[tokio::test]
async fn a_plugin_receives_only_the_settings_it_claimed() {
    static CLAIMING: PluginManifest = PluginManifest {
        id: "test.claiming",
        version: "0",
        sdk: "^0.1",
        provides: &[],
        requires: &[],
        config: Some(ConfigClaim {
            keys: &[
                ("greeting.text", Merge::Replace),
                ("greeting.tags", Merge::Accumulate),
            ],
            schema: || schemars::schema_for!(serde_json::Value),
        }),
    };
    struct Claiming(std::sync::Mutex<Option<Value>>);
    #[async_trait]
    impl Plugin for Claiming {
        fn manifest(&self) -> &'static PluginManifest {
            &CLAIMING
        }
        fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
            *self.0.lock().unwrap() = Some(registrar.config::<Value>()?);
            Ok(())
        }
    }
    let seen: Arc<std::sync::Mutex<Option<Value>>> = Arc::new(std::sync::Mutex::new(None));
    struct Relay(Arc<std::sync::Mutex<Option<Value>>>);
    #[async_trait]
    impl Plugin for Relay {
        fn manifest(&self) -> &'static PluginManifest {
            &CLAIMING
        }
        fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
            *self.0.lock().unwrap() = Some(registrar.config::<Value>()?);
            Ok(())
        }
    }
    let _ = Claiming(std::sync::Mutex::new(None));
    let config = HostConfig::new(env())
        .with_layer(
            "user",
            json!({"greeting": {"text": "hi", "tags": ["a"]}, "model": "m"}),
        )
        .with_layer("project", json!({"greeting": {"tags": ["b"]}}));
    Host::build(vec![Box::new(Relay(Arc::clone(&seen)))], config)
        .await
        .unwrap();
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some(json!({"greeting": {"text": "hi", "tags": ["a", "b"]}}))
    );
}

#[tokio::test]
async fn a_declared_window_is_the_ruler_the_turn_measures_with() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("hello"))]);
    let plugins = vec![TestPlugin::boxed(
        &PROVIDER,
        vec![Contribution::Provider(provider.clone())],
    )];
    let config = HostConfig::new(env())
        .with_layer("user", json!({"model": "m"}))
        .with_layer(
            "project",
            json!({"models": {"scripted/m": {"contextWindow": 50000, "maxOutput": 60000, "reasoning": true}}}),
        );
    let host = Host::build(plugins, config).await.unwrap();
    let Attachment {
        mut events, handle, ..
    } = host
        .open(
            SessionSelector::Create {
                spec: spec("/work"),
            },
            who(),
        )
        .await
        .unwrap();
    handle.submit(IntentId::mint(), Input::text("hi", Origin::surface("test")));
    let mut window = None;
    while let Some(frame) = events.next().await {
        match frame.event {
            Event::TurnUsage { context, .. } => window = Some(context.window),
            Event::TurnCompleted { .. } => break,
            _ => {}
        }
    }
    assert_eq!(
        window,
        Some(25_000),
        "the input side: the window less the output budget"
    );
    assert_eq!(provider.requests()[0].max_tokens, 25_000, "half the window");
}

static STORE: PluginManifest = PluginManifest {
    id: "test.store",
    version: "0",
    sdk: "^0.1",
    provides: &["store:memory"],
    requires: &[],
    config: None,
};

/// A host on a shared store, as a second process would be.
async fn host_on(
    store: Arc<crate::journal::MemoryStore>,
    provider: Arc<ScriptedProvider>,
) -> Arc<Host> {
    let plugins = vec![
        TestPlugin::boxed(&PROVIDER, vec![Contribution::Provider(provider)]),
        TestPlugin::boxed(&STORE, vec![Contribution::Store(store)]),
    ];
    let config = HostConfig::new(env()).with_layer("cli", json!({"model": "m"}));
    Host::build(plugins, config).await.unwrap()
}

/// Submit once and read until the turn completes; the seq of that frame.
async fn one_turn(attachment: &mut Attachment, prompt: &str) -> Seq {
    attachment.handle.submit(
        IntentId::mint(),
        Input::text(prompt, Origin::surface("test")),
    );
    while let Some(frame) = attachment.events.next().await {
        if matches!(frame.event, Event::TurnCompleted { .. }) {
            return frame.seq;
        }
    }
    panic!("the turn never completed");
}

#[tokio::test]
async fn a_stored_session_reopens_on_another_host_with_its_history() {
    let store = Arc::new(crate::journal::MemoryStore::new());
    let first = ScriptedProvider::new(vec![Script::Events(text("first answer"))]);
    let host_a = host_on(store.clone(), first).await;
    let mut a = host_a
        .open(
            SessionSelector::Create {
                spec: spec("/work"),
            },
            who(),
        )
        .await
        .unwrap();
    let ended_at = one_turn(&mut a, "hello").await;
    let id = a.session.clone();

    let second = ScriptedProvider::new(vec![Script::Events(text("second answer"))]);
    let host_b = host_on(store.clone(), second.clone()).await;
    let mut b = host_b
        .open(SessionSelector::ById { id: id.clone() }, who())
        .await
        .unwrap();
    assert_eq!(b.session, id);
    assert!(
        b.snapshot.seq > ended_at,
        "a new head after the old journal"
    );
    assert!(
        b.snapshot
            .items
            .iter()
            .any(|i| matches!(&i.body, ItemBody::Assistant { text } if text == "first answer")),
        "the old items are in the snapshot"
    );
    assert!(!b.snapshot.busy());

    one_turn(&mut b, "again").await;
    let sent = &second.requests()[0].messages;
    assert!(
        sent.iter()
            .any(|m| m.parts.iter().any(|p| p.as_text() == Some("first answer"))),
        "the next request carries the old conversation: {sent:?}"
    );

    let listed = host_b
        .sessions(SessionFilter {
            cwd: Some("/work".into()),
            ..SessionFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(listed.iter().map(|s| &s.id).collect::<Vec<_>>(), [&id]);
}

#[tokio::test]
async fn latest_in_a_directory_comes_from_the_store_when_nothing_is_live() {
    let store = Arc::new(crate::journal::MemoryStore::new());
    let host_a = host_on(
        store.clone(),
        ScriptedProvider::new(vec![Script::Events(text("one"))]),
    )
    .await;
    let mut a = host_a
        .open(
            SessionSelector::Create {
                spec: spec("/work"),
            },
            who(),
        )
        .await
        .unwrap();
    one_turn(&mut a, "hello").await;

    let host_b = host_on(store.clone(), ScriptedProvider::new(vec![])).await;
    let b = host_b
        .open(
            SessionSelector::Latest {
                cwd: "/work".into(),
            },
            who(),
        )
        .await
        .unwrap();
    assert_eq!(b.session, a.session);
    assert_eq!(b.snapshot.summary.model.as_deref(), Some("m"));
    let missing = host_b
        .open(
            SessionSelector::Latest {
                cwd: "/elsewhere".into(),
            },
            who(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(missing.code, ErrorCode::SessionNotFound);
    let unknown = host_b
        .open(
            SessionSelector::ById {
                id: SessionId::from_raw("ses_nope"),
            },
            who(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(unknown.code, ErrorCode::SessionNotFound);
}

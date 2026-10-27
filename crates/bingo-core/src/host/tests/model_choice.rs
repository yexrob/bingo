//! Offline choice transitions, persistence and unchanged request prefixes.
use super::commands::{Client, message};
use super::*;

static COMPACTOR: PluginManifest = PluginManifest {
    id: "test.compactor",
    version: "0",
    uses: &[],
    provides: &["compactor"],
    requires: &[],
    config: None,
};

struct CutAtTail(Arc<ScriptedCompactor>);

#[async_trait]
impl Compactor for CutAtTail {
    async fn compact(
        &self,
        cx: CompactContext<'_>,
        reason: CompactReason,
    ) -> Result<Compaction, CompactError> {
        let boundary = cx.items.last().unwrap().id.clone();
        let mut cut = self.0.compact(cx, reason).await?;
        cut.boundary = boundary;
        cut.summary = "x ".repeat(cut.after as usize * 2);
        Ok(cut)
    }
}

async fn setup(
    home: &std::path::Path,
    scripts: Vec<Script>,
    compactor: Option<Arc<ScriptedCompactor>>,
) -> (Arc<Host>, Arc<ScriptedProvider>) {
    let provider = ScriptedProvider::new(scripts);
    provider.serves(&["large", "small"]);
    let mut plugins = vec![TestPlugin::boxed(
        &PROVIDER,
        vec![Contribution::Provider(provider.clone())],
    )];
    if let Some(compactor) = compactor {
        plugins.push(TestPlugin::boxed(
            &COMPACTOR,
            vec![Contribution::Compactor(Arc::new(CutAtTail(compactor)))],
        ));
    }
    let config = HostConfig::new(env_in(home)).with_layer("test", json!({
        "provider":"scripted", "model":"large", "maxTokens":1000,
        "models":{"scripted/large":{"contextWindow":200000}, "scripted/small":{"contextWindow":8000,"images":false}}
    }));
    (Host::build(plugins, config).await.unwrap(), provider)
}

fn response(used: u64) -> Script {
    let mut events = text("hello");
    if let Ok(ModelEvent::Finish { usage, .. }) = events.last_mut().unwrap() {
        usage.input_tokens = used;
    }
    Script::Events(events)
}

#[tokio::test]
async fn a_smaller_window_compacts_on_the_old_model_before_switching() {
    let home = tempfile::tempdir().unwrap();
    let compactor = ScriptedCompactor::new(vec![ScriptedCompactor::cut("itm_none", 9000, 100)]);
    let (host, _) = setup(home.path(), vec![response(9000)], Some(compactor.clone())).await;
    let mut client = Client::open(&host).await;
    client.ack("hello").await;
    client.until_turn_completed().await;
    let (ack, frames) = client.ack("/model small").await;
    assert!(message(&ack).contains("compacted 9000 → "));
    assert!(client.state.context.unwrap().used < 6300);
    assert_eq!(compactor.requests.lock().unwrap()[0].model, "large");
    assert_eq!(
        host.session_model(&client.state.summary.id)
            .await
            .unwrap()
            .unwrap()
            .id,
        "small"
    );
    let compacted = frames
        .iter()
        .position(|frame| matches!(frame.event, Event::Compacted { .. }))
        .unwrap();
    let changed = frames.iter().position(|frame| matches!(&frame.event, Event::SessionUpdated { summary } if summary.model.as_deref() == Some("small"))).unwrap();
    assert!(compacted < changed);
    host.shutdown().await;
}

#[tokio::test]
async fn unsuccessful_compaction_preserves_the_choice_and_settings() {
    for answer in [
        Err(KernelError::new(ErrorCode::ProviderUnavailable, "offline")),
        Err(KernelError::new(ErrorCode::Interrupted, "cancelled")),
        ScriptedCompactor::cut("itm_none", 9000, 8000),
    ] {
        let home = tempfile::tempdir().unwrap();
        let compactor = ScriptedCompactor::new(vec![answer]);
        let (host, _) = setup(home.path(), vec![response(9000)], Some(compactor)).await;
        let mut client = Client::open(&host).await;
        client.ack("hello").await;
        client.until_turn_completed().await;
        client.ack("/model large").await;
        let path = host.env().data_dir.join("last-model.json");
        let remembered = std::fs::read(&path).unwrap();
        let (ack, _) = client.ack("/model small --keep").await;
        assert_eq!(std::fs::read(&path).unwrap(), remembered);
        assert!(matches!(ack, IntentOutcome::Rejected { .. }), "{ack:?}");
        assert_eq!(
            host.session_model(&client.state.summary.id)
                .await
                .unwrap()
                .unwrap()
                .id,
            "large"
        );
        assert!(!crate::settings::user_path(host.env()).exists());
        host.shutdown().await;
    }
}

#[tokio::test]
async fn pins_round_trip_in_order_without_touching_comments() {
    let home = tempfile::tempdir().unwrap();
    let path = crate::settings::user_path(&env_in(home.path()));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "# Keep this comment\nmodel = \"large\" # chosen before\n",
    )
    .unwrap();
    let (host, _) = setup(home.path(), vec![], None).await;
    let mut client = Client::open(&host).await;
    let mut gateway = host.gateway_events();
    let (ack, _) = client.ack("/model pin scripted/large").await;
    assert_eq!(message(&ack), "pinned scripted/large");
    assert!(matches!(
        gateway.next().await,
        Some(GatewayEvent::CatalogChanged {
            kind: CatalogKind::Models
        })
    ));
    client.ack("/model pin scripted/small").await;
    client.ack("/model pin scripted/large").await;
    assert_eq!(
        crate::host::pins::pinned_models(&host),
        ["scripted/large", "scripted/small"]
    );
    let catalog = host.catalog(CatalogKind::Models).await.unwrap();
    assert_eq!(catalog.entries[0].id, "scripted/large");
    assert_eq!(catalog.entries[1].id, "scripted/small");
    assert_eq!(
        catalog
            .entries
            .iter()
            .find(|entry| entry.id == "scripted/large")
            .unwrap()
            .meta["pinned"],
        true
    );
    client.ack("/model unpin scripted/large").await;
    assert_eq!(crate::host::pins::pinned_models(&host), ["scripted/small"]);
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("# Keep this comment") && saved.contains("# chosen before"));
    let layer = crate::settings::read_layer(crate::settings::LayerOrigin::Explicit, &path);
    assert!(layer.is_ok());
    assert_eq!(
        crate::settings::read_document(&path).unwrap()["pinned"],
        json!(["scripted/small"])
    );
    let (ack, _) = client.ack("/model pin invented").await;
    assert!(
        matches!(ack, IntentOutcome::Rejected { error } if error.message.contains("closest ids:"))
    );
    host.shutdown().await;
}

#[tokio::test]
async fn a_picture_warning_keeps_the_picture_and_the_small_switch_is_instant() {
    let home = tempfile::tempdir().unwrap();
    let (host, _) = setup(home.path(), vec![response(10)], None).await;
    let mut client = Client::open(&host).await;
    client.handle.submit(
        IntentId::mint(),
        Input::Text {
            headline: None,
            text: "look".into(),
            delivery: Delivery::Wake,
            images: vec![Image {
                media_type: "image/png".into(),
                data: "AA==".into(),
                width: None,
                height: None,
                path: None,
            }],
            origin: Origin::surface("test"),
        },
    );
    client.until_turn_completed().await;
    let (ack, frames) = client.ack("/model small").await;
    assert!(message(&ack).contains("pictures stay, small won't see them"));
    assert!(
        !frames
            .iter()
            .any(|frame| matches!(frame.event, Event::TurnStarted { .. }))
    );
    assert!(super::super::model_switch::has_pictures(&client.state));
    host.shutdown().await;
}

#[tokio::test]
async fn a_switch_that_needs_compaction_queues_but_an_ordinary_choice_does_not() {
    tokio::time::timeout(std::time::Duration::from_secs(30), queued_switch())
        .await
        .unwrap();
}

async fn queued_switch() {
    let home = tempfile::tempdir().unwrap();
    let compactor = ScriptedCompactor::new(vec![ScriptedCompactor::cut("itm_none", 20000, 100)]);
    let (host, _) = setup(
        home.path(),
        vec![Script::Hang(vec![ModelEvent::TextStart { id: "b".into() }])],
        Some(compactor.clone()),
    )
    .await;
    let mut client = Client::open(&host).await;
    client.handle.submit(
        IntentId::mint(),
        Input::text("x ".repeat(40000), Origin::surface("test")),
    );
    while let Some(frame) = client.events.next().await {
        client.state.apply(&frame);
        if matches!(
            frame.event,
            Event::ItemCompleted {
                item: Item {
                    body: ItemBody::User { .. },
                    ..
                }
            }
        ) {
            break;
        }
    }
    let (ack, _) = client.ack("/model large").await;
    assert!(message(&ack).contains("this session"));
    let (ack, _) = client.ack("/model small").await;
    assert!(matches!(ack, IntentOutcome::Queued { .. }), "{ack:?}");
    assert!(compactor.calls.lock().unwrap().is_empty());
    client
        .handle
        .interrupt(IntentId::mint(), InterruptScope::Head);
    while let Some(frame) = client.events.next().await {
        client.state.apply(&frame);
        if let Event::IntentAck {
            outcome: IntentOutcome::Applied { result },
            ..
        } = &frame.event
            && result["message"]
                .as_str()
                .is_some_and(|text| text.contains("compacted"))
        {
            break;
        }
    }
    assert_eq!(client.state.summary.model.as_deref(), Some("small"));
    assert_eq!(compactor.requests.lock().unwrap()[0].model, "large");
    host.shutdown().await;
}

#[tokio::test]
async fn catalogue_pins_and_the_same_choice_preserve_request_prefixes() {
    let home = tempfile::tempdir().unwrap();
    let (host, provider) = setup(home.path(), vec![response(10), response(10)], None).await;
    let mut client = Client::open(&host).await;
    client.ack("hello").await;
    client.until_turn_completed().await;
    host.catalog(CatalogKind::Models).await.unwrap();
    client.ack("/model pin scripted/large").await;
    client.ack("/model large").await;
    client.ack("again").await;
    client.until_turn_completed().await;
    let requests = provider.requests();
    assert_eq!(requests[0].system, requests[1].system);
    assert_eq!(requests[0].tools, requests[1].tools);
    assert_eq!(requests[0].provider_options, requests[1].provider_options);
    assert_eq!(requests[0].reasoning, requests[1].reasoning);
    assert_eq!(requests[0].session, requests[1].session);
    assert!(requests[1].messages.starts_with(&requests[0].messages));
    host.shutdown().await;
}

#[tokio::test]
async fn a_small_summary_cannot_hide_a_retained_tail_that_still_overflows() {
    let home = tempfile::tempdir().unwrap();
    let compactor = ScriptedCompactor::new(vec![ScriptedCompactor::cut("itm_none", 9000, 100)]);
    let (host, _) = setup(
        home.path(),
        vec![Script::Events(text(&"x".repeat(40000)))],
        Some(compactor),
    )
    .await;
    let mut client = Client::open(&host).await;
    client.ack("hello").await;
    client.until_turn_completed().await;
    let (ack, _) = client.ack("/model small").await;
    assert!(
        matches!(ack, IntentOutcome::Rejected { error } if error.message.contains("context must be below"))
    );
    assert_eq!(client.state.summary.model.as_deref(), Some("large"));
    assert!(client.state.context.unwrap().used > 6300);
    host.shutdown().await;
}

#[tokio::test]
async fn catalogue_switch_threshold_uses_the_resolved_output_budget() {
    let home = tempfile::tempdir().unwrap();
    let (host, _) = setup(home.path(), vec![], None).await;
    host.refresh_models().await;
    let catalog = host.catalog(CatalogKind::Models).await.unwrap();
    let small = catalog
        .entries
        .iter()
        .find(|e| e.id == "scripted/small")
        .unwrap();
    assert_eq!(small.meta["context"], 8000);
    assert_eq!(small.meta["switchAt"], 6300);
    assert_eq!(small.meta["images"], false);
    assert_eq!(small.meta["limitsSource"], "configured");
    host.shutdown().await;
}

/// A window nobody stated is the fallback guess: the picker draws no switch
/// line for it, yet a session over the guessed trigger still compacts first,
/// because the guess is the only bound the next round's summariser has.
#[tokio::test]
async fn a_guessed_window_draws_no_switch_line_yet_still_compacts() {
    let home = tempfile::tempdir().unwrap();
    let (host, provider) = setup(home.path(), vec![response(10)], None).await;
    provider.serves(&["large", "small", "guess"]);
    host.refresh_models().await;
    let mut client = Client::open(&host).await;
    client.ack(&"word ".repeat(12000)).await;
    client.until_turn_completed().await;
    let catalog = host.catalog(CatalogKind::Models).await.unwrap();
    let guess = catalog.entries.iter().find(|e| e.id == "scripted/guess");
    let guess = guess.unwrap();
    assert!(guess.meta.get("switchAt").is_none(), "{:?}", guess.meta);
    assert!(guess.meta.get("context").is_none(), "{:?}", guess.meta);
    let switches = HostHandle(host.clone())
        .service(&bingo_sdk::switch::MODEL_SWITCH)
        .unwrap();
    let decided = switches.catalog(catalog, &client.state);
    let guess = decided.entries.iter().find(|e| e.id == "scripted/guess");
    assert!(guess.unwrap().meta.get("compacts").is_none());
    let target = crate::host::model_switch::proposed_model(
        &host,
        &client.state.summary.id,
        Some("scripted".into()),
        "guess".into(),
    )
    .await
    .unwrap();
    assert!(!target.window_known);
    let needs = crate::host::model_switch::needs_compaction;
    assert!(!needs(&client.state, &target), "under the guessed trigger");
    if let Some(context) = &mut client.state.context {
        context.used = u64::MAX;
    }
    assert!(needs(&client.state, &target), "over the guessed trigger");
    host.shutdown().await;
}

static REORDER: PluginManifest = PluginManifest {
    id: "test.reorder",
    version: "0",
    uses: &[],
    provides: &[],
    requires: &[],
    config: None,
};

/// A catalogue link may list models in any order it likes.
struct Reversed;
#[async_trait]
impl Interceptor for Reversed {
    fn id(&self) -> &str {
        "reversed"
    }
    fn matcher(&self) -> LinkMatcher {
        LinkMatcher::at(bingo_sdk::catalogue::PROMPT_AUTOCOMPLETE)
    }
    async fn chain(
        &self,
        _: &bingo_sdk::catalogue::Entry,
        _: &HookContext,
        value: Value,
        next: Next<'_, Value, Value>,
    ) -> Result<Value, KernelError> {
        let mut catalog: Catalog = serde_json::from_value(next.run(value).await?).unwrap();
        catalog.entries.reverse();
        Ok(serde_json::to_value(catalog).unwrap())
    }
}

#[tokio::test]
async fn next_walks_the_pins_in_their_setting_order() {
    let home = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![]);
    provider.serves(&["large", "small", "tiny"]);
    let plugins = vec![
        TestPlugin::boxed(&PROVIDER, vec![Contribution::Provider(provider)]),
        TestPlugin::boxed(
            &REORDER,
            vec![Contribution::Interceptor {
                placement: Default::default(),
                external: None,
                link: Arc::new(Reversed),
            }],
        ),
    ];
    let config = HostConfig::new(env_in(home.path())).with_layer(
        "test",
        json!({"provider":"scripted", "model":"large", "maxTokens":1000}),
    );
    let host = Host::build(plugins, config).await.unwrap();
    let mut client = Client::open(&host).await;
    for id in ["large", "tiny", "small", "gone"] {
        crate::host::pins::pin_model(&host, &format!("scripted/{id}"), true).unwrap();
    }
    let mut walked = Vec::new();
    for _ in 0..3 {
        let (ack, _) = client.ack("/model next").await;
        assert!(matches!(ack, IntentOutcome::Applied { .. }), "{ack:?}");
        let model = host.session_model(&client.state.summary.id).await;
        let model = model.unwrap().unwrap().id;
        let saved =
            crate::models::selection::load(&host.env().data_dir.join("last-model.json")).unwrap();
        assert_eq!(saved.provider, "scripted");
        assert_eq!(saved.model, model);
        walked.push(model);
    }
    assert_eq!(walked, ["tiny", "small", "large"]);
    host.shutdown().await;
}

#[tokio::test]
async fn the_published_switch_decision_is_the_one_model_makes() {
    let home = tempfile::tempdir().unwrap();
    let (host, _) = setup(home.path(), vec![response(10)], None).await;
    let mut client = Client::open(&host).await;
    client.ack(&"word ".repeat(12000)).await;
    client.until_turn_completed().await;
    // The last measure lags a transcript that has grown since it was taken.
    if let Some(context) = &mut client.state.context {
        context.used = 10;
    }
    let switches = HostHandle(host.clone())
        .service(&bingo_sdk::switch::MODEL_SWITCH)
        .unwrap();
    let catalog = switches.catalog(
        host.catalog(CatalogKind::Models).await.unwrap(),
        &client.state,
    );
    let compacts =
        |id: &str| catalog.entries.iter().find(|e| e.id == id).unwrap().meta["compacts"].clone();
    assert_eq!(compacts("scripted/small"), json!(true));
    assert_eq!(compacts("scripted/large"), json!(false));
    let small = crate::host::model_switch::proposed_model(
        &host,
        &client.state.summary.id,
        Some("scripted".into()),
        "small".into(),
    )
    .await
    .unwrap();
    assert!(crate::host::model_switch::needs_compaction(
        &client.state,
        &small
    ));
    host.shutdown().await;
}

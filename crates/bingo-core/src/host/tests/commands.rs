//! The kernel's own commands through a real host (ADR-0008 §4).

use super::*;

static CONTEXT: PluginManifest = PluginManifest {
    id: "test.context",
    version: "0",
    sdk: "^0.1",
    provides: &["compactor"],
    requires: &[],
    config: None,
};

/// A host whose two scripted models are declared to reason, so `/think`
/// has something to switch.
async fn host_for(
    scripts: Vec<Script>,
    compactor: Option<Arc<dyn Compactor>>,
) -> (Arc<Host>, Arc<ScriptedProvider>) {
    let provider = ScriptedProvider::new(scripts);
    let mut plugins = vec![TestPlugin::boxed(
        &PROVIDER,
        vec![Contribution::Provider(provider.clone())],
    )];
    if let Some(compactor) = compactor {
        plugins.push(TestPlugin::boxed(
            &CONTEXT,
            vec![Contribution::Compactor(compactor)],
        ));
    }
    let config = HostConfig::new(env()).with_layer(
        "cli",
        json!({
            "model": "m",
            "models": {
                "scripted/m": { "reasoning": true },
                "scripted/m2": { "reasoning": true },
            }
        }),
    );
    (Host::build(plugins, config).await.unwrap(), provider)
}

struct Client {
    state: SessionState,
    events: FrameStream,
    handle: SessionHandle,
}

impl Client {
    async fn open(host: &Host) -> Self {
        let attachment = host
            .open(
                SessionSelector::Create {
                    spec: spec("/work"),
                },
                who(),
            )
            .await
            .unwrap();
        Self {
            state: attachment.snapshot,
            events: attachment.events,
            handle: attachment.handle,
        }
    }

    /// Submit a line and fold frames until its ack; returns the ack and the
    /// frames before it.
    async fn ack(&mut self, line: &str) -> (IntentOutcome, Vec<Frame>) {
        let intent = IntentId::mint();
        self.handle
            .submit(intent.clone(), Input::text(line, Origin::surface("test")));
        let mut seen = Vec::new();
        while let Some(frame) = self.events.next().await {
            self.state.apply(&frame);
            if let Event::IntentAck { intent: i, outcome } = &frame.event
                && i == &intent
            {
                return (outcome.clone(), seen);
            }
            seen.push(frame);
        }
        panic!("the stream ended before the ack");
    }

    async fn until_turn_completed(&mut self) -> Vec<Frame> {
        let mut seen = Vec::new();
        while let Some(frame) = self.events.next().await {
            self.state.apply(&frame);
            let done = matches!(frame.event, Event::TurnCompleted { .. });
            seen.push(frame);
            if done {
                return seen;
            }
        }
        panic!("the stream ended before the turn completed");
    }
}

fn message(outcome: &IntentOutcome) -> String {
    match outcome {
        IntentOutcome::Applied { result } => result["message"].as_str().unwrap().to_string(),
        other => panic!("not applied: {other:?}"),
    }
}

#[tokio::test]
async fn model_and_think_change_the_next_turn_and_are_announced() {
    let (host, provider) = host_for(
        vec![Script::Events(text("hi")), Script::Events(text("again"))],
        None,
    )
    .await;
    let mut client = Client::open(&host).await;
    assert_eq!(client.state.config.kernel, json!({ "thinking": null }));

    let (ack, before) = client.ack("/model m2").await;
    assert_eq!(message(&ack), "model: scripted/m2");
    assert_eq!(client.state.summary.model.as_deref(), Some("m2"));
    assert!(
        before
            .iter()
            .any(|f| matches!(&f.event, Event::SessionUpdated { summary } if summary.model.as_deref() == Some("m2"))),
        "the new model is announced before the ack"
    );
    assert!(
        !before
            .iter()
            .any(|f| matches!(f.event, Event::ConfigChanged { .. })),
        "nothing in the config view changed, so nothing was announced"
    );

    let (ack, _) = client.ack("/think high").await;
    assert_eq!(message(&ack), "thinking: high");
    assert_eq!(client.state.config.kernel, json!({ "thinking": "high" }));

    client.ack("hello").await;
    client.until_turn_completed().await;
    let request = &provider.requests()[0];
    assert_eq!(request.model, "m2");
    assert_eq!(request.reasoning, Some(Effort::High));

    let (ack, _) = client.ack("/think off").await;
    assert_eq!(message(&ack), "thinking: off");
    assert_eq!(client.state.config.kernel, json!({ "thinking": null }));
    client.ack("and again").await;
    client.until_turn_completed().await;
    assert_eq!(
        provider.requests()[1].reasoning,
        None,
        "off means no reasoning parameter on the wire"
    );

    let (ack, _) = client.ack("/think loud").await;
    assert!(
        matches!(ack, IntentOutcome::Rejected { error } if error.code == ErrorCode::InvalidInput)
    );

    let (ack, _) = client.ack("/model").await;
    let IntentOutcome::Applied { result } = ack else {
        panic!("a view");
    };
    assert!(
        result["view"]["text"]
            .as_str()
            .unwrap()
            .starts_with("model: scripted/m2")
    );
}

#[tokio::test]
async fn compact_is_a_turn_of_its_own_carrying_the_instructions() {
    let compactor = ScriptedCompactor::new(vec![ScriptedCompactor::cut("itm_none", 9_000, 100)]);
    let (host, _) = host_for(
        vec![Script::Events(text("one"))],
        Some(compactor.clone() as Arc<dyn Compactor>),
    )
    .await;
    let mut client = Client::open(&host).await;
    client.ack("hi").await;
    client.until_turn_completed().await;

    // The ack follows the command's return; a fast compaction may already
    // have completed by then, so the turn is waited for only if it has not.
    let (ack, mut frames) = client.ack("/compact keep the names").await;
    assert_eq!(message(&ack), "compacting the conversation");
    if !frames
        .iter()
        .any(|f| matches!(f.event, Event::TurnCompleted { .. }))
    {
        frames.extend(client.until_turn_completed().await);
    }
    assert!(frames.iter().any(|f| matches!(
        f.event,
        Event::TurnStarted {
            origin: TurnOrigin::Auto,
            ..
        }
    )));
    assert!(
        frames
            .iter()
            .any(|f| matches!(f.event, Event::Compacted { .. }))
    );
    assert_eq!(client.state.history_generation, 1);
    let calls = compactor.calls.lock().unwrap();
    assert!(matches!(
        &calls[0].0,
        CompactReason::Manual { instructions: Some(i) } if i == "keep the names"
    ));
}

#[tokio::test]
async fn the_catalogue_lists_the_builtins_and_the_models() {
    let (host, _) = host_for(vec![], None).await;
    let commands = host.catalog(CatalogKind::Commands).await.unwrap();
    let names: Vec<&str> = commands.entries.iter().map(|e| e.id.as_str()).collect();
    for name in ["model", "think", "compact"] {
        assert!(names.contains(&name), "{names:?}");
    }
    let models = host.catalog(CatalogKind::Models).await.unwrap();
    assert_eq!(models.entries[0].id, "scripted/m");
}

/// A source's tools and commands are in the catalogue beside the registered
/// ones, a tool's meta riding along (ADR-0009 §1).
#[tokio::test]
async fn the_catalogue_reads_the_sources_too() {
    static SOURCES: PluginManifest = PluginManifest {
        id: "test.sources",
        version: "0",
        sdk: "^0.1",
        provides: &["tools:scripted", "commands:scripted"],
        requires: &[],
        config: None,
    };
    let tools = ScriptedToolSource::new();
    tools.set(vec![Arc::new(EchoTool { read_only: true })]);
    let late = ScriptedCommand::new("late", true, Ok(CommandOutcome::Applied { message: None }));
    let plugins = vec![
        TestPlugin::boxed(
            &PROVIDER,
            vec![Contribution::Provider(ScriptedProvider::new(vec![]))],
        ),
        TestPlugin::boxed(
            &SOURCES,
            vec![
                Contribution::Tools(tools),
                Contribution::Commands(ScriptedCommandSource::new(vec![late])),
            ],
        ),
    ];
    let host = Host::build(plugins, HostConfig::new(env())).await.unwrap();
    let tools = host.catalog(CatalogKind::Tools).await.unwrap();
    let echo = tools
        .entries
        .iter()
        .find(|e| e.id == "Echo")
        .expect("the source's tool");
    assert_eq!(echo.meta["description"], json!("echo"));
    let commands = host.catalog(CatalogKind::Commands).await.unwrap();
    assert!(
        commands.entries.iter().any(|e| e.id == "late"),
        "{commands:?}"
    );
    assert!(
        commands.entries.iter().any(|e| e.id == "model"),
        "the built-ins are there too"
    );
}
